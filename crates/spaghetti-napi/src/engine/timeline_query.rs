//! Read-only RFC 011 canonical timeline, branch, and facet query pack.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::ContentBlock;

use super::detail_query::NamedCount;
use super::query_identity::{
    decode_entity_id, encode_entity_id, FACT_ID_PREFIX, MESSAGE_ID_PREFIX, PROJECT_ID_PREFIX,
    RUN_ID_PREFIX, SESSION_ID_PREFIX,
};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const TIMELINE_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_TIMELINE_PAGE_LIMIT: u32 = 30;
pub const MAX_TIMELINE_PAGE_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TIMELINE_PAGE_LIMIT: u32 = 200;
const MAX_TIMELINE_FILTER_VALUES: usize = 32;
const MAX_TIMELINE_FILTER_VALUE_BYTES: usize = 256;
const MAX_TIMELINE_SEARCH_BYTES: usize = 4 * 1024;
const MAX_TIMELINE_CURSOR_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelinePageRequest {
    pub project_id: String,
    pub session_id: String,
    pub roles: Vec<String>,
    pub native_kinds: Vec<String>,
    pub include_content_kinds: Vec<String>,
    pub include_tool_names: Vec<String>,
    pub exclude_content_kinds: Vec<String>,
    pub exclude_tool_names: Vec<String>,
    pub search: Option<String>,
    /// `all` (default), `root`, `delegated`, or `unknown`.
    pub branch_kind: Option<String>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelinePage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: String,
    pub order: String,
    pub search_syntax: String,
    pub total_is_exact: bool,
    /// Messages matching the request filters, before cursor pagination.
    pub total: u64,
    /// Unfiltered facets for the verified canonical session.
    pub facets: TimelineFacets,
    pub items: Vec<TimelineMessage>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineFacets {
    pub total_messages: u64,
    pub roles: Vec<NamedCount>,
    pub native_kinds: Vec<NamedCount>,
    /// Counts canonical content blocks, not message envelopes.
    pub content_kinds: Vec<NamedCount>,
    /// Counts canonical tool-call blocks. Tool-result blocks have no tool name.
    pub tool_names: Vec<NamedCount>,
    pub branch_kinds: Vec<NamedCount>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineMessage {
    pub message_id: String,
    pub project_id: String,
    pub session_id: String,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub branch_kind: String,
    /// Present only when the decisive delegation relation has a currently
    /// materialized native spawn message.
    pub branch_anchor_message_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_project_key: String,
    pub native_session_id: String,
    pub native_run_id: Option<String>,
    pub native_child_id: Option<String>,
    pub native_task_id: Option<String>,
    pub delegation_kind: Option<String>,
    pub delegation_strength: Option<String>,
    pub delegation_status: Option<String>,
    pub branch_tool_name: Option<String>,
    pub branch_label: Option<String>,
    pub requested_agent_type: Option<String>,
    pub native_message_id: Option<String>,
    pub native_kind: String,
    pub role: String,
    /// Ordered canonical common content blocks. Native source payload is not
    /// copied into timeline pages and remains available from message details.
    pub content: JsonValue,
    pub content_kinds: Vec<String>,
    pub tool_names: Vec<String>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub parent_native_message_id: Option<String>,
    pub model: Option<String>,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineBranchKind {
    All,
    Root,
    Delegated,
    Unknown,
}

impl TimelineBranchKind {
    fn parse(value: Option<&str>) -> Result<Self, EngineError> {
        match value.unwrap_or("all") {
            "all" => Ok(Self::All),
            "root" => Ok(Self::Root),
            "delegated" => Ok(Self::Delegated),
            "unknown" => Ok(Self::Unknown),
            value => Err(EngineError::InvalidQuery(format!(
                "timeline branchKind must be all, root, delegated, or unknown; got {value}"
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
struct ValidatedTimeline {
    project_key: Vec<u8>,
    session_key: Vec<u8>,
    roles: Vec<String>,
    native_kinds: Vec<String>,
    include_content_kinds: Vec<String>,
    include_tool_names: Vec<String>,
    exclude_content_kinds: Vec<String>,
    exclude_tool_names: Vec<String>,
    search_match: Option<String>,
    branch_kind: TimelineBranchKind,
    scope_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TimelineCursor {
    version: u32,
    at_commit_seq: u64,
    scope_hash: String,
    untimed_rank: u8,
    sort_time: String,
    source_object_id: u64,
    source_generation: u64,
    source_cursor: String,
    run_key: String,
    message_key: String,
}

#[derive(Debug)]
struct TimelineRow {
    item: TimelineMessage,
    message_key: Vec<u8>,
    run_key: Vec<u8>,
    untimed_rank: u8,
    sort_time: String,
    source_object_id: u64,
    source_generation: u64,
    source_cursor: Vec<u8>,
    payload_bytes: u64,
}

pub(super) fn validate_timeline_page(request: &TimelinePageRequest) -> Result<(), EngineError> {
    let validated = validate_request(request)?;
    decode_optional_cursor(request.cursor.as_deref(), &validated.scope_hash).map(|_| ())
}

pub(super) fn read_timeline_page(
    connection: &Connection,
    request: &TimelinePageRequest,
) -> Result<TimelinePage, EngineError> {
    let validated = validate_request(request)?;
    let cursor = decode_optional_cursor(request.cursor.as_deref(), &validated.scope_hash)?;
    let cursor_rank = cursor.as_ref().map_or(0, |value| value.untimed_rank);
    let cursor_time = cursor.as_ref().map_or("", |value| value.sort_time.as_str());
    let cursor_source_object_id = cursor
        .as_ref()
        .map_or(Ok(0), |value| sqlite_cursor_integer(value.source_object_id))?;
    let cursor_source_generation = cursor.as_ref().map_or(Ok(0), |value| {
        sqlite_cursor_integer(value.source_generation)
    })?;
    let cursor_source_cursor = decode_cursor_blob(
        cursor.as_ref().map(|value| value.source_cursor.as_str()),
        "source cursor",
        true,
    )?;
    let cursor_run_key = decode_cursor_blob(
        cursor.as_ref().map(|value| value.run_key.as_str()),
        "run identity",
        false,
    )?;
    let cursor_key = decode_cursor_blob(
        cursor.as_ref().map(|value| value.message_key.as_str()),
        "message identity",
        false,
    )?;

    let transaction = begin_snapshot(connection)?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    require_session_membership(&transaction, &validated.project_key, &validated.session_key)?;

    let facets = read_timeline_facets(&transaction, &validated.session_key)?;
    let (from_where_sql, base_arguments) = timeline_from_where(&validated);
    let count_sql = format!("SELECT COUNT(*) {from_where_sql}");
    let count: i64 = transaction
        .query_row(
            &count_sql,
            rusqlite::params_from_iter(base_arguments.iter()),
            |row| row.get(0),
        )
        .map_err(|error| query_sqlite_error("count canonical timeline messages", error))?;
    let total = decode_nonnegative_u64(count, "timeline total")?;

    let mut arguments = base_arguments;
    arguments.push(Value::Integer(i64::from(cursor.is_some())));
    let cursor_present_parameter = arguments.len();
    arguments.push(Value::Integer(i64::from(cursor_rank)));
    let cursor_rank_parameter = arguments.len();
    arguments.push(Value::Text(cursor_time.to_string()));
    let cursor_time_parameter = arguments.len();
    arguments.push(Value::Integer(cursor_source_object_id));
    let cursor_source_object_parameter = arguments.len();
    arguments.push(Value::Integer(cursor_source_generation));
    let cursor_source_generation_parameter = arguments.len();
    arguments.push(Value::Blob(cursor_source_cursor));
    let cursor_source_cursor_parameter = arguments.len();
    arguments.push(Value::Blob(cursor_run_key));
    let cursor_run_parameter = arguments.len();
    arguments.push(Value::Blob(cursor_key));
    let cursor_key_parameter = arguments.len();
    arguments.push(Value::Integer(i64::from(request.limit) + 1));
    let limit_parameter = arguments.len();

    let sql = format!(
        r#"
        WITH timeline_rows AS (
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
                   anchor.message_key AS branch_anchor_message_key,
                   si.adapter_id, fr.source_instance_id,
                   cs.native_project_key, cs.native_session_id,
                   cr.native_run_id,
                   COALESCE(cdm.native_child_id, cd.native_child_id)
                       AS native_child_id,
                   COALESCE(cdm.native_task_id, cd.native_task_id)
                       AS native_task_id,
                   cd.relation_kind, cd.relation_strength,
                   cd.relation_status, cds.tool_name,
                   COALESCE(cd.label, cds.label) AS branch_label,
                   cds.requested_agent_type,
                   cm.native_message_id, cm.native_kind, cm.role,
                   cm.content_json, cm.source_time,
                   cm.source_time_quality,
                   cm.parent_native_message_id, cm.model, cm.fact_id,
                   fr.observed_at, cm.source_object_id,
                   cm.source_generation, cm.cursor_start, cm.last_commit_seq,
                   CASE WHEN cm.source_time IS NULL THEN 1 ELSE 0 END
                       AS untimed_rank,
                   COALESCE(cm.source_time, '') AS sort_time
            {from_where_sql}
        )
        SELECT message_key, session_key, project_key, run_key,
               parent_run_key, branch_kind, branch_anchor_message_key,
               adapter_id, source_instance_id, native_project_key,
               native_session_id, native_run_id, native_child_id,
               native_task_id, relation_kind, relation_strength,
               relation_status, tool_name, branch_label,
               requested_agent_type, native_message_id, native_kind, role,
               content_json, source_time, source_time_quality,
               parent_native_message_id, model, fact_id, observed_at,
               source_object_id, source_generation, cursor_start,
               last_commit_seq,
               untimed_rank, sort_time
        FROM timeline_rows
        WHERE ?{cursor_present_parameter} = 0
           OR untimed_rank > ?{cursor_rank_parameter}
           OR (untimed_rank = ?{cursor_rank_parameter}
               AND sort_time < ?{cursor_time_parameter})
           OR (untimed_rank = ?{cursor_rank_parameter}
               AND sort_time = ?{cursor_time_parameter}
               AND (source_object_id, source_generation, cursor_start,
                    run_key, message_key)
                   < (?{cursor_source_object_parameter},
                      ?{cursor_source_generation_parameter},
                      ?{cursor_source_cursor_parameter},
                      ?{cursor_run_parameter}, ?{cursor_key_parameter}))
        ORDER BY untimed_rank, sort_time DESC, source_object_id DESC,
                 source_generation DESC, cursor_start DESC, run_key DESC,
                 message_key DESC
        LIMIT ?{limit_parameter}
        "#,
    );
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| query_sqlite_error("prepare canonical timeline page", error))?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(arguments.iter()))
        .map_err(|error| query_sqlite_error("execute canonical timeline page", error))?;
    let mut items = Vec::new();
    let mut payload_bytes = 0_u64;
    let mut has_more = false;
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance canonical timeline page", error))?
    {
        if items.len() == request.limit as usize {
            has_more = true;
            break;
        }
        let decoded = decode_timeline_row(row)?;
        let next_payload = payload_bytes
            .checked_add(decoded.payload_bytes)
            .ok_or_else(|| EngineError::Sqlite {
                operation: "bound canonical timeline payload",
                detail: "timeline payload byte count overflowed u64".to_string(),
            })?;
        if next_payload > MAX_TIMELINE_PAGE_PAYLOAD_BYTES {
            if items.is_empty() {
                return Err(EngineError::Sqlite {
                    operation: "bound canonical timeline payload",
                    detail: format!(
                        "one timeline message requires {} payload bytes; maximum is {MAX_TIMELINE_PAGE_PAYLOAD_BYTES}",
                        decoded.payload_bytes
                    ),
                });
            }
            has_more = true;
            break;
        }
        payload_bytes = next_payload;
        items.push(decoded);
    }
    drop(rows);
    drop(statement);
    finish_snapshot(transaction)?;

    let next_cursor = if has_more {
        items
            .last()
            .map(|row| {
                encode_cursor(&TimelineCursor {
                    version: TIMELINE_QUERY_CONTRACT_VERSION,
                    at_commit_seq: watermark,
                    scope_hash: validated.scope_hash.clone(),
                    untimed_rank: row.untimed_rank,
                    sort_time: row.sort_time.clone(),
                    source_object_id: row.source_object_id,
                    source_generation: row.source_generation,
                    source_cursor: URL_SAFE_NO_PAD.encode(&row.source_cursor),
                    run_key: URL_SAFE_NO_PAD.encode(&row.run_key),
                    message_key: URL_SAFE_NO_PAD.encode(&row.message_key),
                })
            })
            .transpose()?
    } else {
        None
    };

    Ok(TimelinePage {
        contract_version: TIMELINE_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        order: "newest_first".to_string(),
        search_syntax: "literal_phrase_v1".to_string(),
        total_is_exact: true,
        total,
        facets,
        items: items.into_iter().map(|row| row.item).collect(),
        payload_bytes,
        payload_byte_limit: MAX_TIMELINE_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

fn validate_request(request: &TimelinePageRequest) -> Result<ValidatedTimeline, EngineError> {
    if !(1..=MAX_TIMELINE_PAGE_LIMIT).contains(&request.limit) {
        return Err(EngineError::InvalidQuery(format!(
            "timeline page limit must be between 1 and {MAX_TIMELINE_PAGE_LIMIT}"
        )));
    }
    let project_key = decode_entity_id(
        &request.project_id,
        PROJECT_ID_PREFIX,
        "timeline project id",
    )?;
    let session_key = decode_entity_id(
        &request.session_id,
        SESSION_ID_PREFIX,
        "timeline session id",
    )?;
    let roles = normalize_filter_values(&request.roles, "roles")?;
    let native_kinds = normalize_filter_values(&request.native_kinds, "nativeKinds")?;
    let include_content_kinds =
        normalize_content_kinds(&request.include_content_kinds, "includeContentKinds")?;
    let include_tool_names =
        normalize_filter_values(&request.include_tool_names, "includeToolNames")?;
    let exclude_content_kinds =
        normalize_content_kinds(&request.exclude_content_kinds, "excludeContentKinds")?;
    let exclude_tool_names =
        normalize_filter_values(&request.exclude_tool_names, "excludeToolNames")?;
    let search = request
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(search) = &search {
        if search.len() > MAX_TIMELINE_SEARCH_BYTES {
            return Err(EngineError::InvalidQuery(format!(
                "timeline search exceeds {MAX_TIMELINE_SEARCH_BYTES} UTF-8 bytes"
            )));
        }
        if search.contains('\0') {
            return Err(EngineError::InvalidQuery(
                "timeline search must not contain NUL".to_string(),
            ));
        }
    }
    let branch_kind = TimelineBranchKind::parse(request.branch_kind.as_deref())?;
    let scope_hash = timeline_scope_hash(
        &project_key,
        &session_key,
        &roles,
        &native_kinds,
        &include_content_kinds,
        &include_tool_names,
        &exclude_content_kinds,
        &exclude_tool_names,
        search.as_deref(),
        branch_kind,
    );
    Ok(ValidatedTimeline {
        project_key,
        session_key,
        roles,
        native_kinds,
        include_content_kinds,
        include_tool_names,
        exclude_content_kinds,
        exclude_tool_names,
        search_match: search.map(|value| literal_match_expression(&value)),
        branch_kind,
        scope_hash,
    })
}

fn normalize_content_kinds(values: &[String], label: &str) -> Result<Vec<String>, EngineError> {
    let values = normalize_filter_values(values, label)?;
    for value in &values {
        if !matches!(
            value.as_str(),
            "text" | "thinking" | "tool_call" | "tool_result" | "image" | "document" | "native"
        ) {
            return Err(EngineError::InvalidQuery(format!(
                "timeline {label} contains unsupported content kind {value}"
            )));
        }
    }
    Ok(values)
}

fn normalize_filter_values(values: &[String], label: &str) -> Result<Vec<String>, EngineError> {
    if values.len() > MAX_TIMELINE_FILTER_VALUES {
        return Err(EngineError::InvalidQuery(format!(
            "timeline {label} exceeds {MAX_TIMELINE_FILTER_VALUES} values"
        )));
    }
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        if value.is_empty()
            || value.trim() != value
            || value.len() > MAX_TIMELINE_FILTER_VALUE_BYTES
            || value.contains('\0')
        {
            return Err(EngineError::InvalidQuery(format!(
                "timeline {label} values must be non-empty, unpadded, NUL-free, and at most {MAX_TIMELINE_FILTER_VALUE_BYTES} UTF-8 bytes"
            )));
        }
        normalized.push(value.clone());
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

#[allow(clippy::too_many_arguments)]
fn timeline_scope_hash(
    project_key: &[u8],
    session_key: &[u8],
    roles: &[String],
    native_kinds: &[String],
    include_content_kinds: &[String],
    include_tool_names: &[String],
    exclude_content_kinds: &[String],
    exclude_tool_names: &[String],
    search: Option<&str>,
    branch_kind: TimelineBranchKind,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_component(&mut hasher, b"canonical-timeline-v1");
    hash_component(&mut hasher, project_key);
    hash_component(&mut hasher, session_key);
    hash_string_values(&mut hasher, roles);
    hash_string_values(&mut hasher, native_kinds);
    hash_string_values(&mut hasher, include_content_kinds);
    hash_string_values(&mut hasher, include_tool_names);
    hash_string_values(&mut hasher, exclude_content_kinds);
    hash_string_values(&mut hasher, exclude_tool_names);
    hash_optional_component(&mut hasher, search.map(str::as_bytes));
    hash_component(&mut hasher, branch_kind.as_str().as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize().as_bytes())
}

fn timeline_from_where(validated: &ValidatedTimeline) -> (String, Vec<Value>) {
    let mut arguments = vec![
        Value::Blob(validated.session_key.clone()),
        Value::Blob(validated.project_key.clone()),
    ];
    let mut conditions = vec![
        "cm.session_key = ?1".to_string(),
        "cs.project_key = ?2".to_string(),
    ];
    push_text_filter(&mut conditions, &mut arguments, "cm.role", &validated.roles);
    push_text_filter(
        &mut conditions,
        &mut arguments,
        "cm.native_kind",
        &validated.native_kinds,
    );
    push_block_include_filter(
        &mut conditions,
        &mut arguments,
        &validated.include_content_kinds,
        &validated.include_tool_names,
    );
    push_block_exclude_filter(
        &mut conditions,
        &mut arguments,
        "content_kind",
        &validated.exclude_content_kinds,
    );
    push_block_exclude_filter(
        &mut conditions,
        &mut arguments,
        "tool_name",
        &validated.exclude_tool_names,
    );
    if let Some(search_match) = &validated.search_match {
        arguments.push(Value::Text(search_match.clone()));
        conditions.push(format!(
            "cm.rowid IN (SELECT rowid FROM canonical_message_search_fts WHERE canonical_message_search_fts MATCH ?{})",
            arguments.len()
        ));
    }
    match validated.branch_kind {
        TimelineBranchKind::All => {}
        TimelineBranchKind::Root => conditions.push(
            "cr.run_key IS NOT NULL AND cr.parent_run_key IS NULL AND cd.child_run_key IS NULL"
                .to_string(),
        ),
        TimelineBranchKind::Delegated => conditions
            .push("(cd.child_run_key IS NOT NULL OR cr.parent_run_key IS NOT NULL)".to_string()),
        TimelineBranchKind::Unknown => {
            conditions.push("cr.run_key IS NULL AND cd.child_run_key IS NULL".to_string());
        }
    }
    let sql = format!(
        r#"
        FROM canonical_messages cm
        JOIN canonical_sessions cs ON cs.session_key = cm.session_key
        JOIN fact_records fr ON fr.fact_id = cm.fact_id
        JOIN source_instances si
          ON si.source_instance_id = fr.source_instance_id
        LEFT JOIN canonical_runs cr
          ON cr.run_key = cm.run_key AND cr.session_key = cm.session_key
        LEFT JOIN canonical_delegations cd
          ON cd.child_run_key = cm.run_key
        LEFT JOIN canonical_delegation_metadata cdm
          ON cdm.child_run_key = cm.run_key
        LEFT JOIN canonical_delegation_spawns cds
          ON cds.decisive_fact_id = cd.decisive_spawn_fact_id
        LEFT JOIN canonical_messages anchor
          ON anchor.message_key = cds.parent_message_key
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
    let placeholders = push_text_arguments(arguments, values);
    conditions.push(format!("{column} IN ({})", placeholders.join(", ")));
}

fn push_block_include_filter(
    conditions: &mut Vec<String>,
    arguments: &mut Vec<Value>,
    content_kinds: &[String],
    tool_names: &[String],
) {
    if content_kinds.is_empty() && tool_names.is_empty() {
        return;
    }
    let mut clauses = Vec::new();
    if !content_kinds.is_empty() {
        let placeholders = push_text_arguments(arguments, content_kinds);
        clauses.push(format!(
            "block.content_kind IN ({})",
            placeholders.join(", ")
        ));
    }
    if !tool_names.is_empty() {
        let placeholders = push_text_arguments(arguments, tool_names);
        clauses.push(format!("block.tool_name IN ({})", placeholders.join(", ")));
    }
    conditions.push(format!(
        "EXISTS (SELECT 1 FROM canonical_message_content_blocks block WHERE block.message_key = cm.message_key AND ({}))",
        clauses.join(" OR ")
    ));
}

fn push_block_exclude_filter(
    conditions: &mut Vec<String>,
    arguments: &mut Vec<Value>,
    column: &str,
    values: &[String],
) {
    if values.is_empty() {
        return;
    }
    let placeholders = push_text_arguments(arguments, values);
    conditions.push(format!(
        "NOT EXISTS (SELECT 1 FROM canonical_message_content_blocks block WHERE block.message_key = cm.message_key AND block.{column} IN ({}))",
        placeholders.join(", ")
    ));
}

fn push_text_arguments(arguments: &mut Vec<Value>, values: &[String]) -> Vec<String> {
    let mut placeholders = Vec::with_capacity(values.len());
    for value in values {
        arguments.push(Value::Text(value.clone()));
        placeholders.push(format!("?{}", arguments.len()));
    }
    placeholders
}

fn read_timeline_facets(
    transaction: &Transaction<'_>,
    session_key: &[u8],
) -> Result<TimelineFacets, EngineError> {
    let total_messages: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM canonical_messages WHERE session_key = ?1",
            [session_key],
            |row| row.get(0),
        )
        .map_err(|error| query_sqlite_error("count timeline facet messages", error))?;
    Ok(TimelineFacets {
        total_messages: decode_nonnegative_u64(total_messages, "timeline facet total")?,
        roles: grouped_counts(
            transaction,
            "SELECT role, COUNT(*) FROM canonical_messages WHERE session_key = ?1 GROUP BY role ORDER BY role",
            session_key,
            "read timeline role facets",
        )?,
        native_kinds: grouped_counts(
            transaction,
            "SELECT native_kind, COUNT(*) FROM canonical_messages WHERE session_key = ?1 GROUP BY native_kind ORDER BY native_kind",
            session_key,
            "read timeline native-kind facets",
        )?,
        content_kinds: grouped_counts(
            transaction,
            "SELECT content_kind, COUNT(*) FROM canonical_message_content_blocks WHERE session_key = ?1 GROUP BY content_kind ORDER BY content_kind",
            session_key,
            "read timeline content-kind facets",
        )?,
        tool_names: grouped_counts(
            transaction,
            "SELECT tool_name, COUNT(*) FROM canonical_message_content_blocks WHERE session_key = ?1 AND tool_name IS NOT NULL GROUP BY tool_name ORDER BY tool_name",
            session_key,
            "read timeline tool facets",
        )?,
        branch_kinds: grouped_counts(
            transaction,
            r#"
            SELECT CASE
                     WHEN cd.child_run_key IS NOT NULL
                       OR cr.parent_run_key IS NOT NULL THEN 'delegated'
                     WHEN cr.run_key IS NOT NULL THEN 'root'
                     ELSE 'unknown'
                   END AS branch_kind,
                   COUNT(*)
            FROM canonical_messages cm
            LEFT JOIN canonical_runs cr
              ON cr.run_key = cm.run_key AND cr.session_key = cm.session_key
            LEFT JOIN canonical_delegations cd
              ON cd.child_run_key = cm.run_key
            WHERE cm.session_key = ?1
            GROUP BY branch_kind
            ORDER BY branch_kind
            "#,
            session_key,
            "read timeline branch facets",
        )?,
    })
}

fn grouped_counts(
    transaction: &Transaction<'_>,
    sql: &str,
    session_key: &[u8],
    operation: &'static str,
) -> Result<Vec<NamedCount>, EngineError> {
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| query_sqlite_error(operation, error))?;
    let counts = statement
        .query_map([session_key], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| query_sqlite_error(operation, error))?
        .map(|row| {
            let (name, count) = row.map_err(|error| query_sqlite_error(operation, error))?;
            Ok(NamedCount {
                name,
                count: decode_nonnegative_u64(count, "timeline facet count")?,
            })
        })
        .collect();
    counts
}

fn decode_timeline_row(row: &Row<'_>) -> Result<TimelineRow, EngineError> {
    let message_key: Vec<u8> = query_get(row, 0, "decode timeline message key")?;
    let session_key: Vec<u8> = query_get(row, 1, "decode timeline session key")?;
    let project_key: Vec<u8> = query_get(row, 2, "decode timeline project key")?;
    let run_key: Vec<u8> = query_get(row, 3, "decode timeline run key")?;
    let parent_run_key: Option<Vec<u8>> = query_get(row, 4, "decode timeline parent run")?;
    let anchor_message_key: Option<Vec<u8>> = query_get(row, 6, "decode timeline branch anchor")?;
    let content_json: Vec<u8> = query_get(row, 23, "decode timeline content")?;
    let blocks: Vec<ContentBlock> =
        serde_json::from_slice(&content_json).map_err(|error| EngineError::Sqlite {
            operation: "decode canonical timeline content JSON",
            detail: error.to_string(),
        })?;
    let (content_kinds, tool_names) = summarize_blocks(&blocks);
    let content = serde_json::to_value(blocks).map_err(|error| EngineError::Sqlite {
        operation: "convert canonical timeline content",
        detail: error.to_string(),
    })?;
    let fact_id: Vec<u8> = query_get(row, 28, "decode timeline fact id")?;
    let source_object_id = decode_nonnegative_u64(
        query_get(row, 30, "decode timeline source object")?,
        "timeline source object",
    )?;
    let source_generation = decode_nonnegative_u64(
        query_get(row, 31, "decode timeline source generation")?,
        "timeline source generation",
    )?;
    let source_cursor = query_get(row, 32, "decode timeline source cursor")?;
    let untimed_rank = decode_untimed_rank(query_get(row, 34, "decode timeline time rank")?)?;
    let sort_time = query_get(row, 35, "decode timeline sort time")?;
    Ok(TimelineRow {
        item: TimelineMessage {
            message_id: encode_entity_id(MESSAGE_ID_PREFIX, &message_key),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            run_id: encode_entity_id(RUN_ID_PREFIX, &run_key),
            parent_run_id: parent_run_key
                .as_deref()
                .map(|key| encode_entity_id(RUN_ID_PREFIX, key)),
            branch_kind: query_get(row, 5, "decode timeline branch kind")?,
            branch_anchor_message_id: anchor_message_key
                .as_deref()
                .map(|key| encode_entity_id(MESSAGE_ID_PREFIX, key)),
            adapter_id: query_get(row, 7, "decode timeline adapter id")?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 8, "decode timeline source instance")?,
                "timeline source instance",
            )?,
            native_project_key: query_get(row, 9, "decode timeline native project")?,
            native_session_id: query_get(row, 10, "decode timeline native session")?,
            native_run_id: query_get(row, 11, "decode timeline native run")?,
            native_child_id: query_get(row, 12, "decode timeline native child")?,
            native_task_id: query_get(row, 13, "decode timeline native task")?,
            delegation_kind: query_get(row, 14, "decode timeline delegation kind")?,
            delegation_strength: query_get(row, 15, "decode timeline delegation strength")?,
            delegation_status: query_get(row, 16, "decode timeline delegation status")?,
            branch_tool_name: query_get(row, 17, "decode timeline branch tool")?,
            branch_label: query_get(row, 18, "decode timeline branch label")?,
            requested_agent_type: query_get(row, 19, "decode timeline requested agent type")?,
            native_message_id: query_get(row, 20, "decode timeline native message")?,
            native_kind: query_get(row, 21, "decode timeline native kind")?,
            role: query_get(row, 22, "decode timeline role")?,
            content,
            content_kinds,
            tool_names,
            source_time: query_get(row, 24, "decode timeline source time")?,
            source_time_quality: query_get(row, 25, "decode timeline time quality")?,
            parent_native_message_id: query_get(row, 26, "decode timeline parent message")?,
            model: query_get(row, 27, "decode timeline model")?,
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
            observed_at_unix_ms: query_get(row, 29, "decode timeline observed time")?,
            source_object_id,
            source_generation,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 33, "decode timeline commit")?,
                "timeline commit",
            )?,
        },
        message_key,
        run_key,
        untimed_rank,
        sort_time,
        source_object_id,
        source_generation,
        source_cursor,
        payload_bytes: usize_to_u64(content_json.len(), "timeline content length")?,
    })
}

fn summarize_blocks(blocks: &[ContentBlock]) -> (Vec<String>, Vec<String>) {
    let mut content_kinds = Vec::new();
    let mut tool_names = Vec::new();
    for block in blocks {
        let (kind, tool_name) = match block {
            ContentBlock::Text { .. } => ("text", None),
            ContentBlock::Thinking { .. } => ("thinking", None),
            ContentBlock::ToolCall { name, .. } => ("tool_call", Some(name.as_str())),
            ContentBlock::ToolResult { .. } => ("tool_result", None),
            ContentBlock::Image { .. } => ("image", None),
            ContentBlock::Document { .. } => ("document", None),
            ContentBlock::Native { .. } => ("native", None),
        };
        if !content_kinds.iter().any(|value| value == kind) {
            content_kinds.push(kind.to_string());
        }
        if let Some(tool_name) = tool_name {
            if !tool_names.iter().any(|value| value == tool_name) {
                tool_names.push(tool_name.to_string());
            }
        }
    }
    (content_kinds, tool_names)
}

fn require_session_membership(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    session_key: &[u8],
) -> Result<(), EngineError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM canonical_sessions WHERE session_key = ?1 AND project_key = ?2",
            rusqlite::params![session_key, project_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("verify timeline session membership", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(
            "timeline projectId/sessionId does not identify a current canonical session"
                .to_string(),
        ));
    }
    Ok(())
}

fn literal_match_expression(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
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
    }
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn decode_optional_cursor(
    value: Option<&str>,
    expected_scope_hash: &str,
) -> Result<Option<TimelineCursor>, EngineError> {
    value
        .map(|value| decode_cursor(value, expected_scope_hash))
        .transpose()
}

fn decode_cursor(value: &str, expected_scope_hash: &str) -> Result<TimelineCursor, EngineError> {
    if value.len() > MAX_TIMELINE_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "timeline cursor exceeds the supported size".to_string(),
        ));
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("timeline cursor is not valid base64url".to_string())
    })?;
    let cursor: TimelineCursor = serde_json::from_slice(&bytes)
        .map_err(|_| EngineError::InvalidQuery("timeline cursor is malformed".to_string()))?;
    if cursor.version != TIMELINE_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported timeline cursor version {}",
            cursor.version
        )));
    }
    if cursor.scope_hash != expected_scope_hash {
        return Err(EngineError::InvalidQuery(
            "timeline cursor does not match the request scope".to_string(),
        ));
    }
    if cursor.untimed_rank > 1 || cursor.sort_time.len() > MAX_TIMELINE_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "timeline cursor order key is outside the supported bounds".to_string(),
        ));
    }
    sqlite_cursor_integer(cursor.source_object_id)?;
    sqlite_cursor_integer(cursor.source_generation)?;
    decode_cursor_blob(Some(&cursor.source_cursor), "source cursor", true)?;
    decode_cursor_blob(Some(&cursor.run_key), "run identity", false)?;
    decode_cursor_blob(Some(&cursor.message_key), "message identity", false)?;
    Ok(cursor)
}

fn encode_cursor(cursor: &TimelineCursor) -> Result<String, EngineError> {
    let bytes = serde_json::to_vec(cursor).map_err(|error| EngineError::Sqlite {
        operation: "encode canonical timeline cursor",
        detail: error.to_string(),
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn sqlite_cursor_integer(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| {
        EngineError::InvalidQuery(
            "timeline cursor integer is outside SQLite's supported range".to_string(),
        )
    })
}

fn decode_cursor_blob(
    value: Option<&str>,
    label: &str,
    allow_empty: bool,
) -> Result<Vec<u8>, EngineError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| EngineError::InvalidQuery(format!("timeline cursor {label} is malformed")))?;
    if (!allow_empty && bytes.is_empty()) || bytes.len() > MAX_TIMELINE_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(format!(
            "timeline cursor {label} is malformed"
        )));
    }
    Ok(bytes)
}

fn validate_cursor_watermark(
    cursor: Option<&TimelineCursor>,
    watermark: u64,
) -> Result<(), EngineError> {
    if let Some(cursor) = cursor {
        if cursor.at_commit_seq != watermark {
            return Err(EngineError::InvalidQuery(format!(
                "timeline cursor expired at commit {}; current commit is {watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    Ok(())
}

fn begin_snapshot(connection: &Connection) -> Result<Transaction<'_>, EngineError> {
    connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin canonical timeline snapshot", error))
}

fn finish_snapshot(transaction: Transaction<'_>) -> Result<(), EngineError> {
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish canonical timeline snapshot", error))
}

fn query_get<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    index: usize,
    operation: &'static str,
) -> Result<T, EngineError> {
    row.get(index)
        .map_err(|error| query_sqlite_error(operation, error))
}

fn decode_untimed_rank(value: i64) -> Result<u8, EngineError> {
    match value {
        0 => Ok(0),
        1 => Ok(1),
        _ => Err(EngineError::Sqlite {
            operation: "decode canonical timeline row",
            detail: format!("timeline untimed rank was outside 0 or 1: {value}"),
        }),
    }
}

fn decode_nonnegative_u64(value: i64, label: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode canonical timeline row",
        detail: format!("{label} was negative: {value}"),
    })
}

fn usize_to_u64(value: usize, label: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode canonical timeline row",
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
    use serde_json::json;

    use super::*;
    use crate::core::schema;

    fn insert_fact(
        connection: &Connection,
        fact_id: &[u8],
        entity_key: &[u8],
        kind: &str,
        ordinal: i64,
        observed_at: i64,
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
                ) VALUES (?1, ?2, ?3, 1, 1, 1, 1, x'00', x'01',
                          zeroblob(32), ?4, ?5, x'7B7D', 1)
                "#,
                params![fact_id, kind, entity_key, ordinal, observed_at],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_message(
        connection: &Connection,
        message_key: &[u8],
        run_key: &[u8],
        fact_id: &[u8],
        native_id: &str,
        role: &str,
        native_kind: &str,
        source_time: Option<&str>,
        content: Vec<ContentBlock>,
        search_text: &str,
        ordinal: i64,
    ) {
        insert_fact(
            connection,
            fact_id,
            message_key,
            "message",
            ordinal,
            1_000 + ordinal,
        );
        let content_json = serde_json::to_vec(&content).unwrap();
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
                ) VALUES (?1, x'7331', ?2, ?3, ?4, ?5, ?6, ?7,
                          CASE WHEN ?7 IS NULL THEN NULL ELSE 'native_exact' END,
                          NULL, 'fixture-model', ?8, x'7B7D', ?9, 1, 1,
                          ?10, x'01', 1)
                "#,
                params![
                    message_key,
                    run_key,
                    native_id,
                    native_kind,
                    role,
                    content_json,
                    source_time,
                    search_text,
                    fact_id,
                    ordinal.to_be_bytes().as_slice(),
                ],
            )
            .unwrap();
        for (block_ordinal, block) in content.iter().enumerate() {
            let (kind, tool_name, native_call_id) = match block {
                ContentBlock::Text { .. } => ("text", None, None),
                ContentBlock::Thinking { .. } => ("thinking", None, None),
                ContentBlock::ToolCall {
                    native_id, name, ..
                } => ("tool_call", Some(name.as_str()), Some(native_id.as_str())),
                ContentBlock::ToolResult { native_call_id, .. } => {
                    ("tool_result", None, Some(native_call_id.as_str()))
                }
                ContentBlock::Image { .. } => ("image", None, None),
                ContentBlock::Document { .. } => ("document", None, None),
                ContentBlock::Native { .. } => ("native", None, None),
            };
            connection
                .execute(
                    r#"
                    INSERT INTO canonical_message_content_blocks (
                        message_key, session_key, run_key, block_ordinal,
                        content_kind, tool_name, native_tool_call_id
                    ) VALUES (?1, x'7331', ?2, ?3, ?4, ?5, ?6)
                    "#,
                    params![
                        message_key,
                        run_key,
                        i64::try_from(block_ordinal).unwrap(),
                        kind,
                        tool_name,
                        native_call_id,
                    ],
                )
                .unwrap();
        }
    }

    fn seeded_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO source_instances VALUES
                    (1, 'fixture', x'01', 'Fixture', 1, 1, 1);
                INSERT INTO source_streams (
                    source_stream_id, source_instance_id, stream_key,
                    driver_kind, decoder_key, stream_state, last_commit_seq
                ) VALUES
                    (1, 1, 'fixture-transcripts', 'append_file', 'fixture',
                     'available', 1);
                INSERT INTO source_objects (
                    source_object_id, source_stream_id, object_key,
                    generation, committed_cursor, decoder_contract_version,
                    last_commit_seq, state
                ) VALUES
                    (1, 1, x'01', 1, x'01', 1, 1, 'active');
                INSERT INTO ingest_commits
                    (commit_seq, source_instance_id, reason, started_at,
                     committed_at, fact_count)
                VALUES (1, 1, 'seed', 1, 2, 10);
                "#,
            )
            .unwrap();
        insert_fact(&connection, b"fs", b"s1", "session", 0, 900);
        insert_fact(&connection, b"fr", b"r1", "run", 1, 901);
        insert_fact(&connection, b"fc", b"rc1", "run", 2, 902);
        insert_fact(&connection, b"fsp", b"spawn", "delegation_spawn", 3, 903);
        insert_fact(&connection, b"fmeta", b"rc1", "delegation_metadata", 4, 904);
        connection
            .execute_batch(
                r#"
                INSERT INTO canonical_sessions (
                    session_key, project_key, native_session_id,
                    native_project_key, cwd, git_branch, first_prompt,
                    ai_title, custom_title, source_time,
                    source_time_quality, fact_id, source_object_id,
                    source_generation, cursor_end, last_commit_seq
                ) VALUES (
                    x'7331', x'7031', 'native-s1', 'native-p1', NULL, NULL,
                    NULL, NULL, NULL, NULL, NULL, x'6673', 1, 1, x'01', 1
                );
                INSERT INTO canonical_runs (
                    run_key, session_key, native_run_id, parent_run_key,
                    fact_id, source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES
                    (x'7231', x'7331', 'root-1', NULL, x'6672', 1, 1, x'01', 1),
                    (x'726331', x'7331', 'child-1', x'7231', x'6663', 1, 1, x'01', 1);
                INSERT INTO delegation_spawn_assertions (
                    fact_id, spawn_key, parent_run_key, parent_message_key,
                    session_key, native_task_id, tool_name, label, prompt,
                    requested_agent_type, source_time, source_time_quality,
                    source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES (
                    x'667370', x'737061776e', x'7231', x'6d31', x'7331',
                    'task-one', 'Task', 'worker', 'do work', 'Explore',
                    '2026-08-12T00:00:01.000Z', 'native_exact', 1, 1, x'01', 1
                );
                INSERT INTO canonical_delegation_spawns (
                    spawn_key, parent_run_key, parent_message_key, session_key,
                    native_task_id, tool_name, label, prompt,
                    requested_agent_type, source_time, source_time_quality,
                    spawn_status, decisive_fact_id, assertion_count,
                    competing_spawn_count, parent_present, last_commit_seq
                ) VALUES (
                    x'737061776e', x'7231', x'6d31', x'7331', 'task-one',
                    'Task', 'worker', 'do work', 'Explore',
                    '2026-08-12T00:00:01.000Z', 'native_exact', 'resolved',
                    x'667370', 1, 0, 1, 1
                );
                INSERT INTO delegation_metadata_assertions (
                    fact_id, child_run_key, session_key, native_child_id,
                    agent_type, description, native_name, spawn_depth,
                    worktree_path, native_task_id, source_object_id,
                    source_generation, cursor_end, last_commit_seq
                ) VALUES (
                    x'666d657461', x'726331', x'7331', 'agent-one', 'worker',
                    'worker', NULL, 1, NULL, 'task-one', 1, 1, x'01', 1
                );
                INSERT INTO canonical_delegation_metadata (
                    child_run_key, session_key, native_child_id, agent_type,
                    description, native_name, spawn_depth, worktree_path,
                    native_task_id, metadata_status, decisive_fact_id,
                    assertion_count, competing_metadata_count, run_present,
                    last_commit_seq
                ) VALUES (
                    x'726331', x'7331', 'agent-one', 'worker', 'worker', NULL,
                    1, NULL, 'task-one', 'resolved', x'666d657461', 1, 0, 1, 1
                );
                INSERT INTO canonical_delegations (
                    child_run_key, parent_run_key, session_key, relation_kind,
                    relation_strength, relation_status, native_child_id,
                    native_task_id, label, prompt, cwd, worktree_path,
                    source_time, source_time_quality, decisive_relation_fact_id,
                    decisive_spawn_fact_id, decisive_metadata_fact_id,
                    assertion_count, competing_relation_count, child_present,
                    parent_present, last_commit_seq
                ) VALUES (
                    x'726331', x'7231', x'7331', 'vendor_native_subagent',
                    'native_explicit', 'resolved', 'agent-one', 'task-one',
                    'worker', 'do work', NULL, NULL,
                    '2026-08-12T00:00:01.000Z', 'native_exact', NULL,
                    x'667370', x'666d657461', 1, 0, 1, 1, 1
                );
                "#,
            )
            .unwrap();
        insert_message(
            &connection,
            b"m1",
            b"r1",
            b"fm1",
            "native-m1",
            "user",
            "user",
            Some("2026-08-12T00:00:00.000Z"),
            vec![ContentBlock::Text {
                text: "root marker".to_string(),
            }],
            "root marker",
            10,
        );
        insert_message(
            &connection,
            b"m2",
            b"r1",
            b"fm2",
            "native-m2",
            "assistant",
            "assistant",
            Some("2026-08-12T00:00:01.000Z"),
            vec![
                ContentBlock::Thinking {
                    text: "consider".to_string(),
                    redacted: false,
                },
                ContentBlock::ToolCall {
                    native_id: "tool-one".to_string(),
                    name: "Task".to_string(),
                    input: json!({ "prompt": "do work" }),
                },
            ],
            "consider Task do work",
            11,
        );
        insert_message(
            &connection,
            b"m3",
            b"rc1",
            b"fm3",
            "native-m3",
            "assistant",
            "assistant",
            Some("2026-08-12T00:00:02.000Z"),
            vec![ContentBlock::Text {
                text: "nested unique marker".to_string(),
            }],
            "nested unique marker",
            12,
        );
        insert_message(
            &connection,
            b"m4",
            b"r1",
            b"fm4",
            "native-m4",
            "user",
            "user",
            None,
            vec![ContentBlock::ToolResult {
                native_call_id: "tool-one".to_string(),
                content: json!("done"),
                is_error: false,
            }],
            "done",
            13,
        );
        connection
    }

    fn request(limit: u32) -> TimelinePageRequest {
        TimelinePageRequest {
            project_id: encode_entity_id(PROJECT_ID_PREFIX, b"p1"),
            session_id: encode_entity_id(SESSION_ID_PREFIX, b"s1"),
            roles: Vec::new(),
            native_kinds: Vec::new(),
            include_content_kinds: Vec::new(),
            include_tool_names: Vec::new(),
            exclude_content_kinds: Vec::new(),
            exclude_tool_names: Vec::new(),
            search: None,
            branch_kind: None,
            cursor: None,
            limit,
        }
    }

    #[test]
    fn timeline_returns_exact_facets_and_decisive_branch_anchor() {
        let connection = seeded_connection();
        let page = read_timeline_page(&connection, &request(10)).unwrap();
        assert_eq!(page.contract_version, 1);
        assert_eq!(page.at_commit_seq, 1);
        assert_eq!(page.order, "newest_first");
        assert_eq!(page.total, 4);
        assert_eq!(page.facets.total_messages, 4);
        assert_eq!(
            page.facets.content_kinds,
            vec![
                NamedCount {
                    name: "text".to_string(),
                    count: 2,
                },
                NamedCount {
                    name: "thinking".to_string(),
                    count: 1,
                },
                NamedCount {
                    name: "tool_call".to_string(),
                    count: 1,
                },
                NamedCount {
                    name: "tool_result".to_string(),
                    count: 1,
                },
            ]
        );
        assert_eq!(
            page.facets.tool_names,
            vec![NamedCount {
                name: "Task".to_string(),
                count: 1,
            }]
        );
        assert_eq!(
            page.items[0].native_message_id.as_deref(),
            Some("native-m3")
        );
        assert_eq!(page.items[0].branch_kind, "delegated");
        assert_eq!(
            page.items[0].branch_anchor_message_id,
            Some(encode_entity_id(MESSAGE_ID_PREFIX, b"m1"))
        );
        assert_eq!(page.items[0].native_child_id.as_deref(), Some("agent-one"));
        assert_eq!(page.items[0].branch_tool_name.as_deref(), Some("Task"));
        assert_eq!(
            page.items.last().unwrap().native_message_id.as_deref(),
            Some("native-m4")
        );
    }

    #[test]
    fn timeline_filters_common_blocks_and_literal_search_without_changing_facets() {
        let connection = seeded_connection();
        let mut tools = request(10);
        tools.include_tool_names = vec!["Task".to_string()];
        let tool_page = read_timeline_page(&connection, &tools).unwrap();
        assert_eq!(tool_page.total, 1);
        assert_eq!(tool_page.items[0].tool_names, vec!["Task"]);
        assert_eq!(tool_page.facets.total_messages, 4);

        let mut includes = request(10);
        includes.include_content_kinds = vec!["tool_result".to_string()];
        includes.include_tool_names = vec!["Task".to_string()];
        assert_eq!(read_timeline_page(&connection, &includes).unwrap().total, 2);

        let mut exclude = request(10);
        exclude.exclude_content_kinds = vec!["thinking".to_string()];
        assert_eq!(read_timeline_page(&connection, &exclude).unwrap().total, 3);

        let mut search = request(10);
        search.search = Some("nested unique marker".to_string());
        assert_eq!(read_timeline_page(&connection, &search).unwrap().total, 1);
        search.search = Some("nested OR root".to_string());
        assert_eq!(read_timeline_page(&connection, &search).unwrap().total, 0);

        let mut delegated = request(10);
        delegated.branch_kind = Some("delegated".to_string());
        assert_eq!(
            read_timeline_page(&connection, &delegated).unwrap().total,
            1
        );
    }

    #[test]
    fn timeline_preserves_source_order_when_qualified_times_tie() {
        let connection = seeded_connection();
        connection
            .execute(
                "UPDATE canonical_messages SET source_time = ?1, cursor_start = x'FF' WHERE message_key = x'6D31'",
                ["2026-08-12T00:00:01.000Z"],
            )
            .unwrap();

        let first = read_timeline_page(&connection, &request(2)).unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| item.native_message_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["native-m3", "native-m1"]
        );

        let mut next_request = request(2);
        next_request.cursor = first.next_cursor;
        let second = read_timeline_page(&connection, &next_request).unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|item| item.native_message_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["native-m2", "native-m4"]
        );
    }

    #[test]
    fn timeline_keyset_cursor_is_scope_and_watermark_bound() {
        let connection = seeded_connection();
        let first = read_timeline_page(&connection, &request(1)).unwrap();
        assert_eq!(first.items.len(), 1);
        let cursor = first.next_cursor.unwrap();

        let mut second_request = request(1);
        second_request.cursor = Some(cursor.clone());
        let second = read_timeline_page(&connection, &second_request).unwrap();
        assert_eq!(second.total, 4);
        assert_ne!(first.items[0].message_id, second.items[0].message_id);

        let mut other_scope = request(1);
        other_scope.roles = vec!["assistant".to_string()];
        other_scope.cursor = Some(cursor.clone());
        assert!(matches!(
            read_timeline_page(&connection, &other_scope),
            Err(EngineError::InvalidQuery(_))
        ));

        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (2, 1, 'later', 3, 4, 0)",
                [],
            )
            .unwrap();
        let mut expired = request(1);
        expired.cursor = Some(cursor);
        let error = read_timeline_page(&connection, &expired).unwrap_err();
        assert!(error.to_string().contains("expired"));
    }

    #[test]
    fn timeline_validates_scope_filters_and_remains_query_only() {
        let connection = seeded_connection();
        let before: i64 = connection
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();
        let page = read_timeline_page(&connection, &request(10)).unwrap();
        assert_eq!(page.items.len(), 4);
        let after: i64 = connection
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, before);

        let mut invalid_limit = request(0);
        assert!(matches!(
            read_timeline_page(&connection, &invalid_limit),
            Err(EngineError::InvalidQuery(_))
        ));
        invalid_limit.limit = 10;
        invalid_limit.include_content_kinds = vec!["vendor_magic".to_string()];
        assert!(matches!(
            read_timeline_page(&connection, &invalid_limit),
            Err(EngineError::InvalidQuery(_))
        ));
        let mut mismatch = request(10);
        mismatch.project_id = encode_entity_id(PROJECT_ID_PREFIX, b"other");
        assert!(matches!(
            read_timeline_page(&connection, &mismatch),
            Err(EngineError::InvalidQuery(_))
        ));
    }
}
