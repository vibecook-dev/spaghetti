//! Read-only RFC 011 canonical full-text search query pack.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use super::query_identity::{
    decode_entity_id, encode_entity_id, FACT_ID_PREFIX, MESSAGE_ID_PREFIX, PROJECT_ID_PREFIX,
    RUN_ID_PREFIX, SESSION_ID_PREFIX,
};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const SEARCH_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_SEARCH_PAGE_LIMIT: u32 = 50;
pub const MAX_SEARCH_PAGE_PAYLOAD_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEARCH_PAGE_LIMIT: u32 = 200;
const MAX_SEARCH_TEXT_BYTES: usize = 4 * 1024;
const MAX_SEARCH_FILTER_VALUES: usize = 32;
const MAX_SEARCH_FILTER_VALUE_BYTES: usize = 256;
const MAX_SEARCH_CURSOR_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPageRequest {
    pub text: String,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub adapter_ids: Vec<String>,
    pub roles: Vec<String>,
    pub native_kinds: Vec<String>,
    /// `all` (default), `root`, `delegated`, or `unknown`.
    pub branch_kind: Option<String>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub query_syntax: String,
    pub score_direction: String,
    pub total_is_exact: bool,
    pub total: u64,
    pub items: Vec<SearchHit>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub message_id: String,
    /// Absent while the referenced canonical session endpoint is unresolved.
    pub project_id: Option<String>,
    pub session_id: String,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub branch_kind: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_project_key: Option<String>,
    pub native_session_id: Option<String>,
    /// Absent while the referenced canonical run endpoint is unresolved.
    pub native_run_id: Option<String>,
    pub native_child_id: Option<String>,
    pub native_task_id: Option<String>,
    pub delegation_status: Option<String>,
    pub native_message_id: Option<String>,
    pub native_kind: String,
    pub role: String,
    pub model: Option<String>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    /// Plain text. FTS excerpts are separated with ` … ` and never contain
    /// markup injected by the query engine.
    pub snippet: String,
    /// SQLite FTS5 BM25 rank. Lower values sort first.
    pub score: f64,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchBranchKind {
    All,
    Root,
    Delegated,
    Unknown,
}

impl SearchBranchKind {
    fn parse(value: Option<&str>) -> Result<Self, EngineError> {
        match value.unwrap_or("all") {
            "all" => Ok(Self::All),
            "root" => Ok(Self::Root),
            "delegated" => Ok(Self::Delegated),
            "unknown" => Ok(Self::Unknown),
            value => Err(EngineError::InvalidQuery(format!(
                "search branchKind must be all, root, delegated, or unknown; got {value}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Root => "root",
            Self::Delegated => "delegated",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug)]
struct ValidatedSearch {
    match_expression: String,
    project_key: Option<Vec<u8>>,
    session_key: Option<Vec<u8>>,
    adapter_ids: Vec<String>,
    roles: Vec<String>,
    native_kinds: Vec<String>,
    branch_kind: SearchBranchKind,
    scope_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SearchCursor {
    version: u32,
    at_commit_seq: u64,
    scope_hash: String,
    score: f64,
    message_key: String,
}

pub(super) fn validate_search_page(request: &SearchPageRequest) -> Result<(), EngineError> {
    let validated = validate_request(request)?;
    decode_optional_cursor(request.cursor.as_deref(), &validated.scope_hash).map(|_| ())
}

pub(super) fn read_search_page(
    connection: &Connection,
    request: &SearchPageRequest,
) -> Result<SearchPage, EngineError> {
    let validated = validate_request(request)?;
    let cursor = decode_optional_cursor(request.cursor.as_deref(), &validated.scope_hash)?;
    let cursor_key = cursor_message_key(cursor.as_ref())?;
    let cursor_score = cursor.as_ref().map_or(0.0, |value| value.score);

    let transaction = begin_snapshot(connection)?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    validate_project_session_membership(&transaction, &validated)?;

    let (from_where_sql, base_arguments) = search_from_where(&validated);
    let count_sql = format!("SELECT COUNT(*) {from_where_sql}");
    let count: i64 = transaction
        .query_row(
            &count_sql,
            rusqlite::params_from_iter(base_arguments.iter()),
            |row| row.get(0),
        )
        .map_err(|error| query_sqlite_error("count canonical search hits", error))?;
    let total = decode_nonnegative_u64(count, "search total")?;

    let mut arguments = base_arguments;
    arguments.push(Value::Integer(i64::from(cursor.is_some())));
    let cursor_present_parameter = arguments.len();
    arguments.push(Value::Real(cursor_score));
    let cursor_score_parameter = arguments.len();
    arguments.push(Value::Blob(cursor_key));
    let cursor_key_parameter = arguments.len();
    arguments.push(Value::Integer(i64::from(request.limit) + 1));
    let limit_parameter = arguments.len();

    let sql = format!(
        r#"
        WITH matched AS (
            SELECT cm.message_key, cm.session_key, cs.project_key,
                   cm.run_key,
                   COALESCE(cd.parent_run_key, cr.parent_run_key)
                       AS parent_run_key,
                   CASE
                     WHEN cd.child_run_key IS NOT NULL
                       OR cr.parent_run_key IS NOT NULL THEN 'delegated'
                     WHEN cr.run_key IS NOT NULL THEN 'root'
                     ELSE 'unknown'
                   END AS branch_kind,
                   si.adapter_id,
                   fr.source_instance_id, cs.native_project_key,
                   cs.native_session_id, cr.native_run_id,
                   COALESCE(cdm.native_child_id, cd.native_child_id)
                       AS native_child_id,
                   COALESCE(cdm.native_task_id, cd.native_task_id)
                       AS native_task_id,
                   cd.relation_status, cm.native_message_id,
                   cm.native_kind, cm.role, cm.model, cm.source_time,
                   cm.source_time_quality,
                   snippet(canonical_message_search_fts, 0, '', '', ' … ', 64)
                       AS snippet,
                   canonical_message_search_fts.rank AS score,
                   cm.fact_id, fr.observed_at, fr.source_object_id,
                   fr.source_generation, cm.last_commit_seq
            {from_where_sql}
        )
        SELECT message_key, session_key, project_key, run_key,
               parent_run_key, branch_kind, adapter_id, source_instance_id,
               native_project_key, native_session_id, native_run_id,
               native_child_id, native_task_id, relation_status,
               native_message_id, native_kind, role, model, source_time,
               source_time_quality, snippet, score, fact_id, observed_at,
               source_object_id, source_generation, last_commit_seq
        FROM matched
        WHERE ?{cursor_present_parameter} = 0
           OR score > ?{cursor_score_parameter}
           OR (score = ?{cursor_score_parameter}
               AND message_key > ?{cursor_key_parameter})
        ORDER BY score, message_key
        LIMIT ?{limit_parameter}
        "#,
    );
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| query_sqlite_error("prepare canonical search page", error))?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(arguments.iter()))
        .map_err(|error| query_sqlite_error("execute canonical search page", error))?;
    let mut items = Vec::new();
    let mut payload_bytes = 0_u64;
    let mut has_more = false;
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance canonical search page", error))?
    {
        if items.len() == request.limit as usize {
            has_more = true;
            break;
        }
        let item = decode_search_hit(row)?;
        let snippet_bytes = usize_to_u64(item.snippet.len(), "search snippet length")?;
        let next_payload =
            payload_bytes
                .checked_add(snippet_bytes)
                .ok_or_else(|| EngineError::Sqlite {
                    operation: "bound canonical search payload",
                    detail: "search payload byte count overflowed u64".to_string(),
                })?;
        if next_payload > MAX_SEARCH_PAGE_PAYLOAD_BYTES {
            if items.is_empty() {
                return Err(EngineError::Sqlite {
                    operation: "bound canonical search payload",
                    detail: format!(
                        "one search hit requires {snippet_bytes} payload bytes; maximum is {MAX_SEARCH_PAGE_PAYLOAD_BYTES}"
                    ),
                });
            }
            has_more = true;
            break;
        }
        payload_bytes = next_payload;
        items.push(item);
    }
    drop(rows);
    drop(statement);
    finish_snapshot(transaction)?;

    let next_cursor = if has_more {
        items
            .last()
            .map(|item| {
                encode_cursor(&SearchCursor {
                    version: SEARCH_QUERY_CONTRACT_VERSION,
                    at_commit_seq: watermark,
                    scope_hash: validated.scope_hash.clone(),
                    score: item.score,
                    message_key: entity_key_from_id(&item.message_id)?,
                })
            })
            .transpose()?
    } else {
        None
    };

    Ok(SearchPage {
        contract_version: SEARCH_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        query_syntax: "literal_phrase_v1".to_string(),
        score_direction: "lower_is_better".to_string(),
        total_is_exact: true,
        total,
        items,
        payload_bytes,
        payload_byte_limit: MAX_SEARCH_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

fn validate_request(request: &SearchPageRequest) -> Result<ValidatedSearch, EngineError> {
    if !(1..=MAX_SEARCH_PAGE_LIMIT).contains(&request.limit) {
        return Err(EngineError::InvalidQuery(format!(
            "search page limit must be between 1 and {MAX_SEARCH_PAGE_LIMIT}"
        )));
    }
    let text = request.text.trim();
    if text.is_empty() {
        return Err(EngineError::InvalidQuery(
            "search text must not be empty".to_string(),
        ));
    }
    if text.len() > MAX_SEARCH_TEXT_BYTES {
        return Err(EngineError::InvalidQuery(format!(
            "search text exceeds {MAX_SEARCH_TEXT_BYTES} UTF-8 bytes"
        )));
    }
    if text.contains('\0') {
        return Err(EngineError::InvalidQuery(
            "search text must not contain NUL".to_string(),
        ));
    }
    let project_key = request
        .project_id
        .as_deref()
        .map(|value| decode_entity_id(value, PROJECT_ID_PREFIX, "search project id"))
        .transpose()?;
    let session_key = request
        .session_id
        .as_deref()
        .map(|value| decode_entity_id(value, SESSION_ID_PREFIX, "search session id"))
        .transpose()?;
    let adapter_ids = normalize_filter_values(&request.adapter_ids, "adapterIds")?;
    let roles = normalize_filter_values(&request.roles, "roles")?;
    let native_kinds = normalize_filter_values(&request.native_kinds, "nativeKinds")?;
    let branch_kind = SearchBranchKind::parse(request.branch_kind.as_deref())?;
    let text = text.to_string();
    let scope_hash = search_scope_hash(
        &text,
        project_key.as_deref(),
        session_key.as_deref(),
        &adapter_ids,
        &roles,
        &native_kinds,
        branch_kind,
    );
    Ok(ValidatedSearch {
        match_expression: literal_match_expression(&text),
        project_key,
        session_key,
        adapter_ids,
        roles,
        native_kinds,
        branch_kind,
        scope_hash,
    })
}

fn normalize_filter_values(values: &[String], label: &str) -> Result<Vec<String>, EngineError> {
    if values.len() > MAX_SEARCH_FILTER_VALUES {
        return Err(EngineError::InvalidQuery(format!(
            "search {label} exceeds {MAX_SEARCH_FILTER_VALUES} values"
        )));
    }
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        if value.is_empty() || value.trim() != value || value.len() > MAX_SEARCH_FILTER_VALUE_BYTES
        {
            return Err(EngineError::InvalidQuery(format!(
                "search {label} values must be non-empty, unpadded, and at most {MAX_SEARCH_FILTER_VALUE_BYTES} UTF-8 bytes"
            )));
        }
        if value.contains('\0') {
            return Err(EngineError::InvalidQuery(format!(
                "search {label} values must not contain NUL"
            )));
        }
        normalized.push(value.clone());
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn literal_match_expression(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

fn search_scope_hash(
    text: &str,
    project_key: Option<&[u8]>,
    session_key: Option<&[u8]>,
    adapter_ids: &[String],
    roles: &[String],
    native_kinds: &[String],
    branch_kind: SearchBranchKind,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_component(&mut hasher, b"canonical-search-v1");
    hash_component(&mut hasher, text.as_bytes());
    hash_optional_component(&mut hasher, project_key);
    hash_optional_component(&mut hasher, session_key);
    hash_string_values(&mut hasher, adapter_ids);
    hash_string_values(&mut hasher, roles);
    hash_string_values(&mut hasher, native_kinds);
    hash_component(&mut hasher, branch_kind.as_str().as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize().as_bytes())
}

fn hash_string_values(hasher: &mut blake3::Hasher, values: &[String]) {
    hasher.update(&(values.len() as u64).to_be_bytes());
    for value in values {
        hash_component(hasher, value.as_bytes());
    }
}

fn hash_optional_component(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_component(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    };
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn search_from_where(validated: &ValidatedSearch) -> (String, Vec<Value>) {
    let mut arguments = vec![Value::Text(validated.match_expression.clone())];
    let mut conditions = vec!["canonical_message_search_fts MATCH ?1".to_string()];
    if let Some(project_key) = &validated.project_key {
        arguments.push(Value::Blob(project_key.clone()));
        conditions.push(format!("cs.project_key = ?{}", arguments.len()));
    }
    if let Some(session_key) = &validated.session_key {
        arguments.push(Value::Blob(session_key.clone()));
        conditions.push(format!("cm.session_key = ?{}", arguments.len()));
    }
    push_text_filter(
        &mut conditions,
        &mut arguments,
        "si.adapter_id",
        &validated.adapter_ids,
    );
    push_text_filter(&mut conditions, &mut arguments, "cm.role", &validated.roles);
    push_text_filter(
        &mut conditions,
        &mut arguments,
        "cm.native_kind",
        &validated.native_kinds,
    );
    match validated.branch_kind {
        SearchBranchKind::All => {}
        SearchBranchKind::Root => conditions.push(
            "cr.run_key IS NOT NULL AND cr.parent_run_key IS NULL AND cd.child_run_key IS NULL"
                .to_string(),
        ),
        SearchBranchKind::Delegated => {
            conditions.push(
                "(cd.child_run_key IS NOT NULL OR cr.parent_run_key IS NOT NULL)".to_string(),
            );
        }
        SearchBranchKind::Unknown => {
            conditions.push("cr.run_key IS NULL AND cd.child_run_key IS NULL".to_string())
        }
    }
    let sql = format!(
        r#"
        FROM canonical_message_search_fts
        JOIN canonical_messages cm
          ON cm.rowid = canonical_message_search_fts.rowid
        LEFT JOIN canonical_sessions cs ON cs.session_key = cm.session_key
        LEFT JOIN canonical_runs cr
          ON cr.run_key = cm.run_key AND cr.session_key = cm.session_key
        JOIN fact_records fr ON fr.fact_id = cm.fact_id
        JOIN source_instances si
          ON si.source_instance_id = fr.source_instance_id
        LEFT JOIN canonical_delegations cd
          ON cd.child_run_key = cm.run_key
        LEFT JOIN canonical_delegation_metadata cdm
          ON cdm.child_run_key = cm.run_key
        WHERE {}
        "#,
        conditions.join(" AND ")
    );
    (sql, arguments)
}

fn push_text_filter(
    conditions: &mut Vec<String>,
    arguments: &mut Vec<Value>,
    column: &str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }
    let mut placeholders = Vec::with_capacity(values.len());
    for value in values {
        arguments.push(Value::Text(value.clone()));
        placeholders.push(format!("?{}", arguments.len()));
    }
    conditions.push(format!("{column} IN ({})", placeholders.join(", ")));
}

fn validate_project_session_membership(
    transaction: &Transaction<'_>,
    validated: &ValidatedSearch,
) -> Result<(), EngineError> {
    let (Some(project_key), Some(session_key)) = (&validated.project_key, &validated.session_key)
    else {
        return Ok(());
    };
    let actual_project = transaction
        .query_row(
            "SELECT project_key FROM canonical_sessions WHERE session_key = ?1",
            [session_key],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|error| query_sqlite_error("verify search project session membership", error))?;
    if actual_project.as_deref() != Some(project_key.as_slice()) {
        return Err(EngineError::InvalidQuery(
            "search projectId/sessionId does not identify a current canonical session".to_string(),
        ));
    }
    Ok(())
}

fn decode_search_hit(row: &Row<'_>) -> Result<SearchHit, EngineError> {
    let message_key: Vec<u8> = query_get(row, 0, "decode search message key")?;
    let session_key: Vec<u8> = query_get(row, 1, "decode search session key")?;
    let project_key: Option<Vec<u8>> = query_get(row, 2, "decode search project key")?;
    let run_key: Vec<u8> = query_get(row, 3, "decode search run key")?;
    let parent_run_key: Option<Vec<u8>> = query_get(row, 4, "decode search parent run key")?;
    let branch_kind: String = query_get(row, 5, "decode search branch kind")?;
    let fact_id: Vec<u8> = query_get(row, 22, "decode search fact id")?;
    let score: f64 = query_get(row, 21, "decode search score")?;
    if !score.is_finite() {
        return Err(EngineError::Sqlite {
            operation: "decode canonical search score",
            detail: format!("FTS rank was not finite: {score}"),
        });
    }
    Ok(SearchHit {
        message_id: encode_entity_id(MESSAGE_ID_PREFIX, &message_key),
        project_id: project_key
            .as_deref()
            .map(|key| encode_entity_id(PROJECT_ID_PREFIX, key)),
        session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
        run_id: encode_entity_id(RUN_ID_PREFIX, &run_key),
        parent_run_id: parent_run_key
            .as_deref()
            .map(|key| encode_entity_id(RUN_ID_PREFIX, key)),
        branch_kind,
        adapter_id: query_get(row, 6, "decode search adapter id")?,
        source_instance_id: decode_nonnegative_u64(
            query_get(row, 7, "decode search source instance")?,
            "search source instance",
        )?,
        native_project_key: query_get(row, 8, "decode search native project")?,
        native_session_id: query_get(row, 9, "decode search native session")?,
        native_run_id: query_get(row, 10, "decode search native run")?,
        native_child_id: query_get(row, 11, "decode search native child")?,
        native_task_id: query_get(row, 12, "decode search native task")?,
        delegation_status: query_get(row, 13, "decode search delegation status")?,
        native_message_id: query_get(row, 14, "decode search native message")?,
        native_kind: query_get(row, 15, "decode search native kind")?,
        role: query_get(row, 16, "decode search role")?,
        model: query_get(row, 17, "decode search model")?,
        source_time: query_get(row, 18, "decode search source time")?,
        source_time_quality: query_get(row, 19, "decode search time quality")?,
        snippet: query_get(row, 20, "decode search snippet")?,
        score,
        decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
        observed_at_unix_ms: query_get(row, 23, "decode search observed time")?,
        source_object_id: decode_nonnegative_u64(
            query_get(row, 24, "decode search source object")?,
            "search source object",
        )?,
        source_generation: decode_nonnegative_u64(
            query_get(row, 25, "decode search source generation")?,
            "search source generation",
        )?,
        last_commit_seq: decode_nonnegative_u64(
            query_get(row, 26, "decode search commit")?,
            "search commit",
        )?,
    })
}

fn decode_optional_cursor(
    value: Option<&str>,
    expected_scope_hash: &str,
) -> Result<Option<SearchCursor>, EngineError> {
    value
        .map(|value| decode_cursor(value, expected_scope_hash))
        .transpose()
}

fn decode_cursor(value: &str, expected_scope_hash: &str) -> Result<SearchCursor, EngineError> {
    if value.len() > MAX_SEARCH_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "search cursor exceeds the supported size".to_string(),
        ));
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("search cursor is not valid base64url".to_string())
    })?;
    let cursor: SearchCursor = serde_json::from_slice(&bytes)
        .map_err(|_| EngineError::InvalidQuery("search cursor is malformed".to_string()))?;
    if cursor.version != SEARCH_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported search cursor version {}",
            cursor.version
        )));
    }
    if cursor.scope_hash != expected_scope_hash {
        return Err(EngineError::InvalidQuery(
            "search cursor does not match the request scope".to_string(),
        ));
    }
    if !cursor.score.is_finite() {
        return Err(EngineError::InvalidQuery(
            "search cursor score is not finite".to_string(),
        ));
    }
    let message_key = URL_SAFE_NO_PAD.decode(&cursor.message_key).map_err(|_| {
        EngineError::InvalidQuery("search cursor message identity is malformed".to_string())
    })?;
    if message_key.is_empty() || message_key.len() > MAX_SEARCH_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "search cursor message identity is malformed".to_string(),
        ));
    }
    Ok(cursor)
}

fn encode_cursor(cursor: &SearchCursor) -> Result<String, EngineError> {
    if !cursor.score.is_finite() {
        return Err(EngineError::Sqlite {
            operation: "encode canonical search cursor",
            detail: "FTS rank was not finite".to_string(),
        });
    }
    let bytes = serde_json::to_vec(cursor).map_err(|error| EngineError::Sqlite {
        operation: "encode canonical search cursor",
        detail: error.to_string(),
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn cursor_message_key(cursor: Option<&SearchCursor>) -> Result<Vec<u8>, EngineError> {
    cursor.map_or_else(
        || Ok(Vec::new()),
        |cursor| {
            URL_SAFE_NO_PAD
                .decode(&cursor.message_key)
                .map_err(|_| EngineError::InvalidQuery("search cursor is malformed".to_string()))
        },
    )
}

fn entity_key_from_id(message_id: &str) -> Result<String, EngineError> {
    let key = decode_entity_id(message_id, MESSAGE_ID_PREFIX, "search result message id")?;
    Ok(URL_SAFE_NO_PAD.encode(key))
}

fn validate_cursor_watermark(
    cursor: Option<&SearchCursor>,
    watermark: u64,
) -> Result<(), EngineError> {
    if let Some(cursor) = cursor {
        if cursor.at_commit_seq != watermark {
            return Err(EngineError::InvalidQuery(format!(
                "search cursor expired at commit {}; current commit is {watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    Ok(())
}

fn begin_snapshot(connection: &Connection) -> Result<Transaction<'_>, EngineError> {
    connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin canonical search snapshot", error))
}

fn finish_snapshot(transaction: Transaction<'_>) -> Result<(), EngineError> {
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish canonical search snapshot", error))
}

fn query_get<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    index: usize,
    operation: &'static str,
) -> Result<T, EngineError> {
    row.get(index)
        .map_err(|error| query_sqlite_error(operation, error))
}

fn decode_nonnegative_u64(value: i64, label: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode canonical search row",
        detail: format!("{label} was negative: {value}"),
    })
}

fn usize_to_u64(value: usize, label: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode canonical search row",
        detail: format!("{label} was outside u64"),
    })
}

fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::core::schema;

    fn insert_fact(
        connection: &Connection,
        fact_id: &[u8],
        message_key: &[u8],
        source_instance_id: i64,
        source_object_id: i64,
    ) {
        connection
            .execute(
                r#"
                INSERT INTO fact_records (
                    fact_id, fact_kind, entity_key, source_instance_id,
                    source_stream_id, source_object_id, source_generation,
                    cursor_start, cursor_end, payload_hash,
                    local_fact_ordinal, observed_at, payload_json,
                    last_commit_seq
                ) VALUES (?1, 'message', ?2, ?3, ?3, ?4, 1,
                          x'00', x'01', zeroblob(32), 0, 1234, x'7B7D', 1)
                "#,
                params![fact_id, message_key, source_instance_id, source_object_id],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_message(
        connection: &Connection,
        message_key: &[u8],
        session_key: &[u8],
        run_key: &[u8],
        fact_id: &[u8],
        native_id: &str,
        role: &str,
        native_kind: &str,
        text: &str,
        source_instance_id: i64,
        source_object_id: i64,
    ) {
        insert_fact(
            connection,
            fact_id,
            message_key,
            source_instance_id,
            source_object_id,
        );
        connection
            .execute(
                r#"
                INSERT INTO canonical_messages (
                    message_key, session_key, run_key, native_message_id,
                    native_kind, role, content_json, source_time,
                    source_time_quality, parent_native_message_id, model,
                    search_text, raw_json, fact_id, source_object_id,
                    source_generation, cursor_start, cursor_end,
                    last_commit_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, x'5B5D',
                          '2026-08-12T00:00:00.000Z', 'native_exact', NULL,
                          'fixture-model', ?7, x'7B7D', ?8, ?9, 1,
                          x'00', x'01', 1)
                "#,
                params![
                    message_key,
                    session_key,
                    run_key,
                    native_id,
                    native_kind,
                    role,
                    text,
                    fact_id,
                    source_object_id,
                ],
            )
            .unwrap();
    }

    fn seeded_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO source_instances VALUES
                    (1, 'fixture', x'01', 'Fixture', '1.0.0', 1, '[]', '[]', 1, 1),
                    (2, 'other', x'02', 'Other', '1.0.0', 1, '[]', '[]', 1, 1);
                INSERT INTO source_streams (
                    source_stream_id, source_instance_id, stream_key,
                    driver_kind, decoder_key, stream_state, last_commit_seq
                ) VALUES
                    (1, 1, 'fixture-transcripts', 'append_file', 'fixture',
                     'available', 1),
                    (2, 2, 'other-transcripts', 'append_file', 'other',
                     'available', 1);
                INSERT INTO source_objects (
                    source_object_id, source_stream_id, object_key,
                    generation, committed_cursor, decoder_contract_version,
                    last_commit_seq, state
                ) VALUES
                    (1, 1, x'01', 1, x'01', 1, 1, 'active'),
                    (2, 2, x'02', 1, x'01', 1, 1, 'active');
                INSERT INTO ingest_commits
                    (commit_seq, source_instance_id, reason, started_at,
                     committed_at, fact_count)
                VALUES (1, 1, 'seed', 1, 2, 10);
                INSERT INTO fact_records (
                    fact_id, fact_kind, entity_key, source_instance_id,
                    source_stream_id, source_object_id, source_generation,
                    cursor_start, cursor_end, payload_hash,
                    local_fact_ordinal, observed_at, payload_json,
                    last_commit_seq
                ) VALUES
                    (x'11', 'session', x'7331', 1, 1, 1, 1, x'00', x'01',
                     zeroblob(32), 0, 1234, x'7B7D', 1),
                    (x'12', 'session', x'7332', 2, 2, 2, 1, x'00', x'01',
                     zeroblob(32), 0, 1234, x'7B7D', 1),
                    (x'21', 'run', x'7231', 1, 1, 1, 1, x'00', x'01',
                     zeroblob(32), 1, 1234, x'7B7D', 1),
                    (x'22', 'run', x'726331', 1, 1, 1, 1, x'00', x'01',
                     zeroblob(32), 2, 1234, x'7B7D', 1),
                    (x'23', 'run', x'7232', 2, 2, 2, 1, x'00', x'01',
                     zeroblob(32), 1, 1234, x'7B7D', 1),
                    (x'31', 'delegation_metadata', x'726331', 1, 1, 1, 1,
                     x'00', x'01', zeroblob(32), 3, 1234, x'7B7D', 1);
                INSERT INTO canonical_sessions (
                    session_key, project_key, native_session_id,
                    native_project_key, cwd, git_branch, first_prompt,
                    ai_title, custom_title, source_time,
                    source_time_quality, fact_id, source_object_id,
                    source_generation, cursor_end, last_commit_seq
                ) VALUES
                    (x'7331', x'7031', 'native-s1', 'native-p1', NULL, NULL,
                     NULL, NULL, NULL, NULL, NULL, x'11', 1, 1, x'01', 1),
                    (x'7332', x'7032', 'native-s2', 'native-p2', NULL, NULL,
                     NULL, NULL, NULL, NULL, NULL, x'12', 2, 1, x'01', 1);
                INSERT INTO canonical_runs (
                    run_key, session_key, native_run_id, parent_run_key,
                    fact_id, source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES
                    (x'7231', x'7331', 'root-1', NULL, x'21', 1, 1, x'01', 1),
                    (x'726331', x'7331', 'child-1', x'7231', x'22', 1, 1, x'01', 1),
                    (x'7232', x'7332', 'root-2', NULL, x'23', 2, 1, x'01', 1);
                INSERT INTO delegation_metadata_assertions (
                    fact_id, child_run_key, session_key, native_child_id,
                    agent_type, description, native_name, spawn_depth,
                    worktree_path, native_task_id, source_object_id,
                    source_generation, cursor_end, last_commit_seq
                ) VALUES (
                    x'31', x'726331', x'7331', 'agent-one', 'worker', NULL,
                    NULL, 1, NULL, 'task-one', 1, 1, x'01', 1
                );
                INSERT INTO canonical_delegation_metadata (
                    child_run_key, session_key, native_child_id, agent_type,
                    description, native_name, spawn_depth, worktree_path,
                    native_task_id, metadata_status, decisive_fact_id,
                    assertion_count, competing_metadata_count, run_present,
                    last_commit_seq
                ) VALUES (
                    x'726331', x'7331', 'agent-one', 'worker', NULL, NULL, 1,
                    NULL, 'task-one', 'resolved', x'31', 1, 0, 1, 1
                );
                "#,
            )
            .unwrap();
        insert_message(
            &connection,
            b"m1",
            b"s1",
            b"r1",
            b"f1",
            "native-m1",
            "user",
            "user",
            "alpha common phrase",
            1,
            1,
        );
        insert_message(
            &connection,
            b"m2",
            b"s1",
            b"rc1",
            b"f2",
            "native-m2",
            "assistant",
            "assistant",
            "alpha common phrase delegated unique",
            1,
            1,
        );
        insert_message(
            &connection,
            b"m3",
            b"s2",
            b"r2",
            b"f3",
            "native-m3",
            "user",
            "user",
            "alpha common phrase",
            2,
            2,
        );
        insert_message(
            &connection,
            b"m4",
            b"s2",
            b"r2",
            b"f4",
            "native-m4",
            "user",
            "user",
            "alpha OR beta",
            2,
            2,
        );
        insert_message(
            &connection,
            b"m5",
            b"pending-session",
            b"pending-run",
            b"f5",
            "native-m5",
            "assistant",
            "assistant",
            "pending endpoint marker",
            1,
            1,
        );
        connection
    }

    fn request(text: &str, limit: u32) -> SearchPageRequest {
        SearchPageRequest {
            text: text.to_string(),
            project_id: None,
            session_id: None,
            adapter_ids: Vec::new(),
            roles: Vec::new(),
            native_kinds: Vec::new(),
            branch_kind: None,
            cursor: None,
            limit,
        }
    }

    #[test]
    fn canonical_search_merges_root_and_delegated_hits_with_stable_keyset_pages() {
        let connection = seeded_connection();
        let first = read_search_page(&connection, &request("alpha common phrase", 1)).unwrap();
        assert_eq!(first.contract_version, 1);
        assert_eq!(first.at_commit_seq, 1);
        assert_eq!(first.query_syntax, "literal_phrase_v1");
        assert_eq!(first.score_direction, "lower_is_better");
        assert!(first.total_is_exact);
        assert_eq!(first.total, 3);
        assert_eq!(first.items.len(), 1);
        assert!(first.next_cursor.is_some());
        assert!(!first.items[0].snippet.contains('<'));

        let mut second_request = request("alpha common phrase", 1);
        second_request.cursor = first.next_cursor.clone();
        let second = read_search_page(&connection, &second_request).unwrap();
        assert_eq!(second.total, 3);
        assert_eq!(second.items.len(), 1);
        assert!(second.next_cursor.is_some());
        assert_ne!(first.items[0].message_id, second.items[0].message_id);

        let mut third_request = request("alpha common phrase", 1);
        third_request.cursor = second.next_cursor.clone();
        let third = read_search_page(&connection, &third_request).unwrap();
        assert_eq!(third.total, 3);
        assert_eq!(third.items.len(), 1);
        assert!(third.next_cursor.is_none());
        let ids = [
            first.items[0].message_id.clone(),
            second.items[0].message_id.clone(),
            third.items[0].message_id.clone(),
        ];
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            3
        );

        let delegated = [first, second, third]
            .into_iter()
            .flat_map(|page| page.items)
            .find(|hit| hit.branch_kind == "delegated")
            .unwrap();
        assert_eq!(delegated.native_child_id.as_deref(), Some("agent-one"));
        assert_eq!(delegated.native_task_id.as_deref(), Some("task-one"));
        assert!(delegated.parent_run_id.is_some());
    }

    #[test]
    fn canonical_search_filters_are_common_and_literal() {
        let connection = seeded_connection();
        let mut delegated = request("alpha common phrase", 10);
        delegated.branch_kind = Some("delegated".to_string());
        assert_eq!(read_search_page(&connection, &delegated).unwrap().total, 1);

        let mut project = request("alpha common phrase", 10);
        project.project_id = Some(encode_entity_id(PROJECT_ID_PREFIX, b"p1"));
        assert_eq!(read_search_page(&connection, &project).unwrap().total, 2);

        let mut adapter = request("alpha common phrase", 10);
        adapter.adapter_ids = vec!["other".to_string()];
        assert_eq!(read_search_page(&connection, &adapter).unwrap().total, 1);

        let mut role = request("alpha common phrase", 10);
        role.roles = vec!["assistant".to_string()];
        let role_page = read_search_page(&connection, &role).unwrap();
        assert_eq!(role_page.total, 1);
        assert_eq!(role_page.items[0].role, "assistant");

        let literal = read_search_page(&connection, &request("alpha OR beta", 10)).unwrap();
        assert_eq!(literal.total, 1, "OR must be searched as literal text");
        assert_eq!(
            literal.items[0].native_message_id.as_deref(),
            Some("native-m4")
        );

        connection
            .execute(
                r#"UPDATE canonical_messages SET search_text = 'alpha OR beta "quoted"' WHERE message_key = ?1"#,
                [b"m4".as_slice()],
            )
            .unwrap();
        let quoted =
            read_search_page(&connection, &request("alpha OR beta \"quoted\"", 10)).unwrap();
        assert_eq!(quoted.total, 1, "quotes must remain literal query text");
    }

    #[test]
    fn canonical_search_cursor_is_scope_and_watermark_bound() {
        let connection = seeded_connection();
        let first = read_search_page(&connection, &request("alpha common phrase", 1)).unwrap();
        let cursor = first.next_cursor.unwrap();

        let mut other_scope = request("delegated unique", 1);
        other_scope.cursor = Some(cursor.clone());
        assert!(matches!(
            read_search_page(&connection, &other_scope),
            Err(EngineError::InvalidQuery(_))
        ));

        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (2, 1, 'later', 3, 4, 0)",
                [],
            )
            .unwrap();
        let mut expired = request("alpha common phrase", 1);
        expired.cursor = Some(cursor);
        let error = read_search_page(&connection, &expired).unwrap_err();
        assert!(matches!(error, EngineError::InvalidQuery(_)));
        assert!(error.to_string().contains("expired"));
    }

    #[test]
    fn canonical_search_rejects_invalid_requests_and_mismatched_membership() {
        let connection = seeded_connection();
        assert!(matches!(
            read_search_page(&connection, &request("   ", 1)),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            read_search_page(&connection, &request("alpha", 0)),
            Err(EngineError::InvalidQuery(_))
        ));
        let mut invalid_branch = request("alpha", 1);
        invalid_branch.branch_kind = Some("childish".to_string());
        assert!(matches!(
            read_search_page(&connection, &invalid_branch),
            Err(EngineError::InvalidQuery(_))
        ));
        let mut invalid_filter = request("alpha", 1);
        invalid_filter.roles = vec![" user ".to_string()];
        assert!(matches!(
            read_search_page(&connection, &invalid_filter),
            Err(EngineError::InvalidQuery(_))
        ));
        let mut mismatch = request("alpha", 1);
        mismatch.project_id = Some(encode_entity_id(PROJECT_ID_PREFIX, b"p2"));
        mismatch.session_id = Some(encode_entity_id(SESSION_ID_PREFIX, b"s1"));
        assert!(matches!(
            read_search_page(&connection, &mismatch),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn canonical_search_index_tracks_updates_and_deletes_in_writer_transactions() {
        let connection = seeded_connection();
        assert_eq!(
            read_search_page(&connection, &request("delegated unique", 10))
                .unwrap()
                .total,
            1
        );
        connection
            .execute(
                "UPDATE canonical_messages SET search_text = 'replacement marker' WHERE message_key = ?1",
                [b"m2".as_slice()],
            )
            .unwrap();
        assert_eq!(
            read_search_page(&connection, &request("delegated unique", 10))
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            read_search_page(&connection, &request("replacement marker", 10))
                .unwrap()
                .total,
            1
        );
        connection
            .execute(
                "DELETE FROM canonical_messages WHERE message_key = ?1",
                [b"m2".as_slice()],
            )
            .unwrap();
        assert_eq!(
            read_search_page(&connection, &request("replacement marker", 10))
                .unwrap()
                .total,
            0
        );

        let plan = connection
            .prepare(
                "EXPLAIN QUERY PLAN SELECT rowid FROM canonical_message_search_fts \
                 WHERE canonical_message_search_fts MATCH ?1",
            )
            .unwrap()
            .query_map(["replacement"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(plan
            .iter()
            .any(|detail| detail.contains("VIRTUAL TABLE INDEX")));
    }

    #[test]
    fn canonical_search_keeps_messages_with_pending_relation_endpoints() {
        let connection = seeded_connection();
        let page = read_search_page(&connection, &request("pending endpoint marker", 10)).unwrap();
        assert_eq!(page.total, 1);
        let hit = &page.items[0];
        assert_eq!(hit.project_id, None);
        assert_eq!(hit.native_project_key, None);
        assert_eq!(hit.native_session_id, None);
        assert_eq!(hit.native_run_id, None);
        assert_eq!(hit.branch_kind, "unknown");

        let mut unknown = request("pending endpoint marker", 10);
        unknown.branch_kind = Some("unknown".to_string());
        assert_eq!(read_search_page(&connection, &unknown).unwrap().total, 1);

        let mut root = request("pending endpoint marker", 10);
        root.branch_kind = Some("root".to_string());
        assert_eq!(read_search_page(&connection, &root).unwrap().total, 0);
    }
}
