//! Read-only RFC 011 detail, source-inventory, and statistics query pack.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::query_identity::{
    decode_entity_id, encode_entity_id, FACT_ID_PREFIX, MESSAGE_ID_PREFIX, PROJECT_ID_PREFIX,
    SESSION_ID_PREFIX, SOURCE_ID_PREFIX,
};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const DETAIL_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_DETAIL_PAGE_LIMIT: u32 = 50;
const MAX_DETAIL_PAGE_LIMIT: u32 = 200;
const MAX_DETAIL_CURSOR_BYTES: usize = 32 * 1024;
/// The sum of canonical content JSON and native payload JSON returned in one
/// message page. Row metadata adds only bounded scalar overhead.
pub const MAX_MESSAGE_PAGE_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDetailsRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDetails {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub session: Option<SessionDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDetail {
    pub session_id: String,
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_session_id: String,
    pub native_project_key: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub message_count: u64,
    pub run_count: u64,
    pub presence_count: u64,
    pub task_collection_count: u64,
    pub artifact_count: u64,
    pub workflow_count: u64,
    pub persisted_tool_result_count: u64,
    pub project_memory_document_count: u64,
    pub index: Option<SessionIndexDetail>,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndexDetail {
    pub full_path: String,
    pub file_mtime_ms: u64,
    pub first_prompt: String,
    pub summary: Option<String>,
    pub message_count: u64,
    pub created_at: String,
    pub created_at_quality: String,
    pub modified_at: String,
    pub modified_at_quality: String,
    pub git_branch: String,
    pub project_path: String,
    pub is_sidechain: bool,
    pub transcript_status: String,
    pub resolution_status: String,
    pub assertion_count: u64,
    pub competing_entry_count: u64,
    pub identity_conflict: bool,
    pub join_conflict: bool,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePageRequest {
    pub project_id: String,
    pub session_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<MessageDetail>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDetail {
    pub message_id: String,
    pub session_id: String,
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_session_id: String,
    pub native_project_key: String,
    pub native_message_id: Option<String>,
    pub native_kind: String,
    pub role: String,
    pub content: JsonValue,
    pub native_payload: JsonValue,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub parent_native_message_id: Option<String>,
    pub model: Option<String>,
    pub search_text: Option<String>,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePageRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub items: Vec<SourceSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSummary {
    pub source_id: String,
    pub source_instance_id: u64,
    pub adapter_id: String,
    pub display_name: String,
    pub adapter_version: String,
    pub adapter_contract_version: u32,
    pub source_schema_versions: Vec<String>,
    pub capabilities: Vec<SourceCapabilitySummary>,
    pub discovered_at_unix_ms: i64,
    pub last_seen_at_unix_ms: i64,
    pub stream_count: u64,
    pub unavailable_stream_count: u64,
    pub object_count: u64,
    pub active_object_count: u64,
    pub record_error_count: u64,
    pub fact_count: u64,
    pub commit_count: u64,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCapabilitySummary {
    pub id: String,
    pub support_level: String,
    pub granularity: String,
    pub availability: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalStats {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub schema_version: u32,
    pub source_instances: u64,
    pub source_streams: u64,
    pub source_objects: u64,
    pub active_source_objects: u64,
    pub source_record_errors: u64,
    pub ingest_commits: u64,
    pub fact_records: u64,
    pub searchable_messages: u64,
    pub entities: Vec<NamedCount>,
    pub source_stream_states: Vec<NamedCount>,
    pub projection_readiness: Vec<NamedCount>,
    pub database_page_count: u64,
    pub database_page_size_bytes: u64,
    pub allocated_database_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedCount {
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MessageCursor {
    version: u32,
    at_commit_seq: u64,
    project_id: String,
    session_id: String,
    untimed_rank: u8,
    sort_time: String,
    message_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceCursor {
    version: u32,
    at_commit_seq: u64,
    adapter_id: String,
    source_instance_id: u64,
}

#[derive(Debug)]
struct MessageRow {
    detail: MessageDetail,
    message_key: Vec<u8>,
    untimed_rank: u8,
    sort_time: String,
    payload_bytes: u64,
}

#[derive(Debug)]
struct SourceRow {
    summary: SourceSummary,
}

pub(super) fn validate_session_details(
    request: &SessionDetailsRequest,
) -> Result<Vec<u8>, EngineError> {
    decode_entity_id(&request.session_id, SESSION_ID_PREFIX, "session detail id")
}

pub(super) fn validate_message_page(
    request: &MessagePageRequest,
) -> Result<(Vec<u8>, Vec<u8>), EngineError> {
    validate_page_limit(request.limit, "message")?;
    let project_key =
        decode_entity_id(&request.project_id, PROJECT_ID_PREFIX, "message project id")?;
    let session_key =
        decode_entity_id(&request.session_id, SESSION_ID_PREFIX, "message session id")?;
    request
        .cursor
        .as_deref()
        .map(|cursor| decode_message_cursor(cursor, request))
        .transpose()?;
    Ok((project_key, session_key))
}

pub(super) fn validate_source_page(request: &SourcePageRequest) -> Result<(), EngineError> {
    validate_page_limit(request.limit, "source")?;
    let cursor = request
        .cursor
        .as_deref()
        .map(decode_source_cursor)
        .transpose()?;
    if let Some(cursor) = cursor {
        to_query_i64(cursor.source_instance_id, "source cursor instance id")?;
    }
    Ok(())
}

pub(super) fn read_session_details(
    connection: &Connection,
    request: &SessionDetailsRequest,
) -> Result<SessionDetails, EngineError> {
    let session_key = validate_session_details(request)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin session detail snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let session = transaction
        .query_row(
            r#"
            SELECT cs.session_key, cs.project_key, cs.native_session_id,
                   cs.native_project_key, cs.cwd, cs.git_branch,
                   cs.first_prompt, cs.ai_title, cs.custom_title,
                   cs.source_time, cs.source_time_quality, si.adapter_id,
                   fr.source_instance_id, cs.fact_id, fr.observed_at,
                   cs.source_object_id, cs.source_generation,
                   (SELECT COUNT(*) FROM canonical_messages cm
                    WHERE cm.session_key = cs.session_key) AS message_count,
                   (SELECT COUNT(*) FROM canonical_runs cr
                    WHERE cr.session_key = cs.session_key) AS run_count,
                   (SELECT COUNT(*) FROM canonical_presences cp
                    WHERE cp.session_key = cs.session_key) AS presence_count,
                   (SELECT COUNT(*) FROM canonical_task_collections ctc
                    WHERE ctc.session_key = cs.session_key) AS task_collection_count,
                   (SELECT COUNT(*) FROM canonical_artifacts ca
                    WHERE ca.session_key = cs.session_key) AS artifact_count,
                   (SELECT COUNT(*) FROM canonical_workflows cw
                    WHERE cw.session_key = cs.session_key) AS workflow_count,
                   (SELECT COUNT(*) FROM canonical_persisted_tool_results ctr
                    WHERE ctr.session_key = cs.session_key) AS tool_result_count,
                   (SELECT COUNT(*) FROM canonical_project_memory_documents md
                    WHERE md.project_key = cs.project_key) AS memory_document_count,
                   csi.full_path, csi.file_mtime_ms,
                   csi.first_prompt AS index_first_prompt, csi.summary,
                   csi.message_count AS index_message_count, csi.created_at,
                   csi.created_at_quality, csi.modified_at,
                   csi.modified_at_quality, csi.git_branch AS index_git_branch,
                   csi.project_path, csi.is_sidechain,
                   csi.transcript_status, csi.resolution_status,
                   csi.assertion_count, csi.competing_entry_count,
                   csi.identity_conflict, csi.join_conflict,
                   csi.last_commit_seq AS index_commit_seq,
                   MAX(
                       cs.last_commit_seq,
                       COALESCE(csi.last_commit_seq, 0),
                       COALESCE((SELECT MAX(cm.last_commit_seq)
                                 FROM canonical_messages cm
                                 WHERE cm.session_key = cs.session_key), 0),
                       COALESCE((SELECT MAX(cr.last_commit_seq)
                                 FROM canonical_runs cr
                                 WHERE cr.session_key = cs.session_key), 0),
                       COALESCE((SELECT MAX(cp.last_commit_seq)
                                 FROM canonical_presences cp
                                 WHERE cp.session_key = cs.session_key), 0),
                       COALESCE((SELECT MAX(ctc.last_commit_seq)
                                 FROM canonical_task_collections ctc
                                 WHERE ctc.session_key = cs.session_key), 0),
                       COALESCE((SELECT MAX(ca.last_commit_seq)
                                 FROM canonical_artifacts ca
                                 WHERE ca.session_key = cs.session_key), 0),
                       COALESCE((SELECT MAX(cw.last_commit_seq)
                                 FROM canonical_workflows cw
                                 WHERE cw.session_key = cs.session_key), 0),
                       COALESCE((SELECT MAX(ctr.last_commit_seq)
                                 FROM canonical_persisted_tool_results ctr
                                 WHERE ctr.session_key = cs.session_key), 0),
                       COALESCE((SELECT MAX(md.last_commit_seq)
                                 FROM canonical_project_memory_documents md
                                 WHERE md.project_key = cs.project_key), 0)
                   ) AS last_commit_seq
            FROM canonical_sessions cs
            JOIN fact_records fr ON fr.fact_id = cs.fact_id
            JOIN source_instances si
              ON si.source_instance_id = fr.source_instance_id
            LEFT JOIN canonical_session_index_entries csi
              ON csi.session_key = cs.session_key
            WHERE cs.session_key = ?1
            "#,
            [&session_key],
            decode_session_detail,
        )
        .optional()
        .map_err(|error| query_sqlite_error("read session details", error))?;
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish session detail snapshot", error))?;
    Ok(SessionDetails {
        contract_version: DETAIL_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        session,
    })
}

fn decode_session_detail(row: &Row<'_>) -> rusqlite::Result<SessionDetail> {
    let session_key = row.get::<_, Vec<u8>>(0)?;
    let project_key = row.get::<_, Vec<u8>>(1)?;
    let fact_id = row.get::<_, Vec<u8>>(13)?;
    let index_path = row.get::<_, Option<String>>(25)?;
    let index = index_path
        .map(|full_path| -> rusqlite::Result<SessionIndexDetail> {
            Ok(SessionIndexDetail {
                full_path,
                file_mtime_ms: sql_u64(row.get(26)?, 26)?,
                first_prompt: row.get(27)?,
                summary: row.get(28)?,
                message_count: sql_u64(row.get(29)?, 29)?,
                created_at: row.get(30)?,
                created_at_quality: row.get(31)?,
                modified_at: row.get(32)?,
                modified_at_quality: row.get(33)?,
                git_branch: row.get(34)?,
                project_path: row.get(35)?,
                is_sidechain: row.get::<_, i64>(36)? != 0,
                transcript_status: row.get(37)?,
                resolution_status: row.get(38)?,
                assertion_count: sql_u64(row.get(39)?, 39)?,
                competing_entry_count: sql_u64(row.get(40)?, 40)?,
                identity_conflict: row.get::<_, i64>(41)? != 0,
                join_conflict: row.get::<_, i64>(42)? != 0,
                last_commit_seq: sql_u64(row.get(43)?, 43)?,
            })
        })
        .transpose()?;
    Ok(SessionDetail {
        session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
        project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
        native_session_id: row.get(2)?,
        native_project_key: row.get(3)?,
        cwd: row.get(4)?,
        git_branch: row.get(5)?,
        first_prompt: row.get(6)?,
        ai_title: row.get(7)?,
        custom_title: row.get(8)?,
        source_time: row.get(9)?,
        source_time_quality: row.get(10)?,
        adapter_id: row.get(11)?,
        source_instance_id: sql_u64(row.get(12)?, 12)?,
        decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
        observed_at_unix_ms: row.get(14)?,
        source_object_id: sql_u64(row.get(15)?, 15)?,
        source_generation: sql_u64(row.get(16)?, 16)?,
        message_count: sql_u64(row.get(17)?, 17)?,
        run_count: sql_u64(row.get(18)?, 18)?,
        presence_count: sql_u64(row.get(19)?, 19)?,
        task_collection_count: sql_u64(row.get(20)?, 20)?,
        artifact_count: sql_u64(row.get(21)?, 21)?,
        workflow_count: sql_u64(row.get(22)?, 22)?,
        persisted_tool_result_count: sql_u64(row.get(23)?, 23)?,
        project_memory_document_count: sql_u64(row.get(24)?, 24)?,
        index,
        last_commit_seq: sql_u64(row.get(44)?, 44)?,
    })
}

pub(super) fn read_message_page(
    connection: &Connection,
    request: &MessagePageRequest,
) -> Result<MessagePage, EngineError> {
    let (project_key, session_key) = validate_message_page(request)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|value| decode_message_cursor(value, request))
        .transpose()?;
    let cursor_key = cursor_message_key(cursor.as_ref())?;
    let cursor_rank = cursor.as_ref().map_or(0, |cursor| cursor.untimed_rank);
    let cursor_time = cursor
        .as_ref()
        .map(|cursor| cursor.sort_time.as_str())
        .unwrap_or("");
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin message page snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(
        cursor.as_ref().map(|cursor| cursor.at_commit_seq),
        watermark,
        "message",
    )?;
    require_session_membership(&transaction, &project_key, &session_key)?;

    let mut statement = transaction
        .prepare(
            r#"
            WITH message_rows AS (
                SELECT cm.message_key, cm.session_key, cs.project_key,
                       cm.native_message_id, cm.native_kind, cm.role,
                       cm.content_json, cm.source_time,
                       cm.source_time_quality, cm.parent_native_message_id,
                       cm.model, cm.search_text, cm.raw_json, cm.fact_id,
                       cm.source_object_id, cm.source_generation,
                       cm.last_commit_seq, si.adapter_id,
                       fr.source_instance_id, fr.observed_at,
                       cs.native_session_id, cs.native_project_key,
                       CASE WHEN cm.source_time IS NULL THEN 1 ELSE 0 END
                           AS untimed_rank,
                       COALESCE(cm.source_time, '') AS sort_time
                FROM canonical_messages cm
                JOIN canonical_sessions cs ON cs.session_key = cm.session_key
                JOIN fact_records fr ON fr.fact_id = cm.fact_id
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
                WHERE cm.session_key = ?1 AND cs.project_key = ?2
            )
            SELECT message_key, session_key, project_key, native_message_id,
                   native_kind, role, content_json, source_time,
                   source_time_quality, parent_native_message_id, model,
                   search_text, raw_json, fact_id, source_object_id,
                   source_generation, last_commit_seq, adapter_id,
                   source_instance_id, observed_at, native_session_id,
                   native_project_key, untimed_rank, sort_time
            FROM message_rows
            WHERE (?3 = 0)
               OR untimed_rank > ?4
               OR (untimed_rank = ?4 AND sort_time > ?5)
               OR (untimed_rank = ?4 AND sort_time = ?5
                   AND message_key > ?6)
            ORDER BY untimed_rank, sort_time, message_key
            LIMIT ?7
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare message page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            session_key,
            project_key,
            i64::from(cursor.is_some()),
            i64::from(cursor_rank),
            cursor_time,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute message page", error))?;

    let mut messages = Vec::new();
    let mut payload_bytes = 0_u64;
    let mut has_more = false;
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance message page", error))?
    {
        if messages.len() >= request.limit as usize {
            has_more = true;
            break;
        }
        let decoded = decode_message_row(row)?;
        let next_bytes = payload_bytes
            .checked_add(decoded.payload_bytes)
            .ok_or_else(|| EngineError::Sqlite {
                operation: "bound message page payload",
                detail: "message payload byte total overflowed u64".to_string(),
            })?;
        if next_bytes > MAX_MESSAGE_PAGE_PAYLOAD_BYTES {
            if messages.is_empty() {
                return Err(EngineError::Sqlite {
                    operation: "bound message page payload",
                    detail: format!(
                        "one canonical message requires {} payload bytes; maximum is {MAX_MESSAGE_PAGE_PAYLOAD_BYTES}",
                        decoded.payload_bytes
                    ),
                });
            }
            has_more = true;
            break;
        }
        payload_bytes = next_bytes;
        messages.push(decoded);
    }
    drop(rows);
    drop(statement);
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish message page snapshot", error))?;

    let next_cursor = if has_more {
        messages
            .last()
            .map(|row| {
                encode_message_cursor(&MessageCursor {
                    version: DETAIL_QUERY_CONTRACT_VERSION,
                    at_commit_seq: watermark,
                    project_id: request.project_id.clone(),
                    session_id: request.session_id.clone(),
                    untimed_rank: row.untimed_rank,
                    sort_time: row.sort_time.clone(),
                    message_key: URL_SAFE_NO_PAD.encode(&row.message_key),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(MessagePage {
        contract_version: DETAIL_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        items: messages.into_iter().map(|row| row.detail).collect(),
        payload_bytes,
        payload_byte_limit: MAX_MESSAGE_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

fn decode_message_row(row: &Row<'_>) -> Result<MessageRow, EngineError> {
    let message_key: Vec<u8> = query_get(row, 0, "decode message key")?;
    let session_key: Vec<u8> = query_get(row, 1, "decode message session key")?;
    let project_key: Vec<u8> = query_get(row, 2, "decode message project key")?;
    let content_json: Vec<u8> = query_get(row, 6, "decode canonical message content")?;
    let raw_json: Vec<u8> = query_get(row, 12, "decode native message payload")?;
    let fact_id: Vec<u8> = query_get(row, 13, "decode message fact id")?;
    let payload_bytes = u64::try_from(content_json.len())
        .ok()
        .and_then(|left| {
            u64::try_from(raw_json.len())
                .ok()
                .and_then(|right| left.checked_add(right))
        })
        .ok_or_else(|| EngineError::Sqlite {
            operation: "bound message row payload",
            detail: "message payload length exceeded u64".to_string(),
        })?;
    let content = serde_json::from_slice(&content_json).map_err(|error| EngineError::Sqlite {
        operation: "decode canonical message content JSON",
        detail: error.to_string(),
    })?;
    let native_payload =
        serde_json::from_slice(&raw_json).map_err(|error| EngineError::Sqlite {
            operation: "decode native message payload JSON",
            detail: error.to_string(),
        })?;
    let untimed_rank = decode_nonnegative_u8(
        query_get(row, 22, "decode message untimed rank")?,
        "message untimed rank",
    )?;
    let sort_time = query_get(row, 23, "decode message order time")?;
    Ok(MessageRow {
        detail: MessageDetail {
            message_id: encode_entity_id(MESSAGE_ID_PREFIX, &message_key),
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            native_message_id: query_get(row, 3, "decode native message id")?,
            native_kind: query_get(row, 4, "decode native message kind")?,
            role: query_get(row, 5, "decode message role")?,
            content,
            native_payload,
            source_time: query_get(row, 7, "decode message source time")?,
            source_time_quality: query_get(row, 8, "decode message time quality")?,
            parent_native_message_id: query_get(row, 9, "decode parent native message id")?,
            model: query_get(row, 10, "decode message model")?,
            search_text: query_get(row, 11, "decode message search text")?,
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
            source_object_id: decode_nonnegative_u64(
                query_get(row, 14, "decode message source object")?,
                "message source object id",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 15, "decode message source generation")?,
                "message source generation",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 16, "decode message commit sequence")?,
                "message commit sequence",
            )?,
            adapter_id: query_get(row, 17, "decode message adapter id")?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 18, "decode message source instance")?,
                "message source instance id",
            )?,
            observed_at_unix_ms: query_get(row, 19, "decode message observation time")?,
            native_session_id: query_get(row, 20, "decode message native session id")?,
            native_project_key: query_get(row, 21, "decode message native project key")?,
        },
        message_key,
        untimed_rank,
        sort_time,
        payload_bytes,
    })
}

pub(super) fn read_source_page(
    connection: &Connection,
    request: &SourcePageRequest,
) -> Result<SourcePage, EngineError> {
    validate_source_page(request)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(decode_source_cursor)
        .transpose()?;
    let cursor_adapter = cursor
        .as_ref()
        .map(|cursor| cursor.adapter_id.as_str())
        .unwrap_or("");
    let cursor_instance = cursor
        .as_ref()
        .map_or(0, |cursor| cursor.source_instance_id);
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin source page snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(
        cursor.as_ref().map(|cursor| cursor.at_commit_seq),
        watermark,
        "source",
    )?;
    let mut statement = transaction
        .prepare(
            r#"
            WITH stream_stats AS (
                SELECT source_instance_id, COUNT(*) AS stream_count,
                       SUM(CASE WHEN stream_state = 'available'
                                THEN 0 ELSE 1 END) AS unavailable_stream_count
                FROM source_streams GROUP BY source_instance_id
            ),
            object_stats AS (
                SELECT ss.source_instance_id, COUNT(*) AS object_count,
                       SUM(CASE WHEN so.state = 'active'
                                THEN 1 ELSE 0 END) AS active_object_count
                FROM source_objects so
                JOIN source_streams ss
                  ON ss.source_stream_id = so.source_stream_id
                GROUP BY ss.source_instance_id
            ),
            error_stats AS (
                SELECT ss.source_instance_id, COUNT(*) AS error_count
                FROM source_record_errors sre
                JOIN source_objects so
                  ON so.source_object_id = sre.source_object_id
                JOIN source_streams ss
                  ON ss.source_stream_id = so.source_stream_id
                GROUP BY ss.source_instance_id
            ),
            fact_stats AS (
                SELECT source_instance_id, COUNT(*) AS fact_count
                FROM fact_records GROUP BY source_instance_id
            ),
            commit_stats AS (
                SELECT source_instance_id, COUNT(*) AS commit_count,
                       MAX(commit_seq) AS last_commit_seq
                FROM ingest_commits WHERE committed_at IS NOT NULL
                GROUP BY source_instance_id
            )
            SELECT si.source_instance_id, si.adapter_id, si.display_name,
                   si.adapter_version, si.adapter_contract_version,
                   si.source_schema_versions_json, si.capabilities_json,
                   si.discovered_at, si.last_seen_at,
                   COALESCE(ss.stream_count, 0),
                   COALESCE(ss.unavailable_stream_count, 0),
                   COALESCE(os.object_count, 0),
                   COALESCE(os.active_object_count, 0),
                   COALESCE(es.error_count, 0), COALESCE(fs.fact_count, 0),
                   COALESCE(cs.commit_count, 0), cs.last_commit_seq
            FROM source_instances si
            LEFT JOIN stream_stats ss
              ON ss.source_instance_id = si.source_instance_id
            LEFT JOIN object_stats os
              ON os.source_instance_id = si.source_instance_id
            LEFT JOIN error_stats es
              ON es.source_instance_id = si.source_instance_id
            LEFT JOIN fact_stats fs
              ON fs.source_instance_id = si.source_instance_id
            LEFT JOIN commit_stats cs
              ON cs.source_instance_id = si.source_instance_id
            WHERE (?1 = 0)
               OR si.adapter_id > ?2
               OR (si.adapter_id = ?2 AND si.source_instance_id > ?3)
            ORDER BY si.adapter_id, si.source_instance_id
            LIMIT ?4
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare source page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            i64::from(cursor.is_some()),
            cursor_adapter,
            to_query_i64(cursor_instance, "source cursor instance id")?,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute source page", error))?;
    let mut sources = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance source page", error))?
    {
        sources.push(decode_source_row(row)?);
    }
    drop(rows);
    drop(statement);
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish source page snapshot", error))?;

    let has_more = sources.len() > request.limit as usize;
    if has_more {
        sources.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        sources
            .last()
            .map(|row| {
                encode_source_cursor(&SourceCursor {
                    version: DETAIL_QUERY_CONTRACT_VERSION,
                    at_commit_seq: watermark,
                    adapter_id: row.summary.adapter_id.clone(),
                    source_instance_id: row.summary.source_instance_id,
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(SourcePage {
        contract_version: DETAIL_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        items: sources.into_iter().map(|row| row.summary).collect(),
        next_cursor,
    })
}

fn decode_source_row(row: &Row<'_>) -> Result<SourceRow, EngineError> {
    let source_instance_id = decode_nonnegative_u64(
        query_get(row, 0, "decode source instance id")?,
        "source instance id",
    )?;
    let source_key = source_instance_id.to_be_bytes();
    Ok(SourceRow {
        summary: SourceSummary {
            source_id: encode_entity_id(SOURCE_ID_PREFIX, &source_key),
            source_instance_id,
            adapter_id: query_get(row, 1, "decode source adapter id")?,
            display_name: query_get(row, 2, "decode source display name")?,
            adapter_version: query_get(row, 3, "decode source adapter version")?,
            adapter_contract_version: decode_nonnegative_u32(
                query_get(row, 4, "decode source contract version")?,
                "source adapter contract version",
            )?,
            source_schema_versions: decode_source_manifest_json(
                query_get(row, 5, "decode source schema versions")?,
                "source schema versions",
            )?,
            capabilities: decode_source_manifest_json(
                query_get(row, 6, "decode source capabilities")?,
                "source capabilities",
            )?,
            discovered_at_unix_ms: query_get(row, 7, "decode source discovery time")?,
            last_seen_at_unix_ms: query_get(row, 8, "decode source last-seen time")?,
            stream_count: decode_nonnegative_u64(
                query_get(row, 9, "decode source stream count")?,
                "source stream count",
            )?,
            unavailable_stream_count: decode_nonnegative_u64(
                query_get(row, 10, "decode unavailable source stream count")?,
                "unavailable source stream count",
            )?,
            object_count: decode_nonnegative_u64(
                query_get(row, 11, "decode source object count")?,
                "source object count",
            )?,
            active_object_count: decode_nonnegative_u64(
                query_get(row, 12, "decode active source object count")?,
                "active source object count",
            )?,
            record_error_count: decode_nonnegative_u64(
                query_get(row, 13, "decode source record error count")?,
                "source record error count",
            )?,
            fact_count: decode_nonnegative_u64(
                query_get(row, 14, "decode source fact count")?,
                "source fact count",
            )?,
            commit_count: decode_nonnegative_u64(
                query_get(row, 15, "decode source commit count")?,
                "source commit count",
            )?,
            last_commit_seq: decode_optional_u64(
                query_get(row, 16, "decode source last commit")?,
                "source last commit sequence",
            )?,
        },
    })
}

fn decode_source_manifest_json<T>(value: String, label: &'static str) -> Result<T, EngineError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(&value).map_err(|error| EngineError::Sqlite {
        operation: "decode source manifest",
        detail: format!("invalid {label}: {error}"),
    })
}

pub(super) fn read_canonical_stats(connection: &Connection) -> Result<CanonicalStats, EngineError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin canonical statistics snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let schema_version = super::super::core::schema::current_schema_version(&transaction)
        .map_err(|error| EngineError::Sqlite {
            operation: "read statistics schema version",
            detail: error.to_string(),
        })?
        .unwrap_or(0);
    let project_count = query_count(
        &transaction,
        r#"
        SELECT COUNT(DISTINCT project_key) FROM (
            SELECT project_key FROM canonical_sessions
            UNION ALL
            SELECT project_key FROM canonical_session_indexes
            UNION ALL
            SELECT project_key FROM canonical_project_memory_documents
        )
        "#,
        "count canonical projects",
    )?;
    let entity_tables = [
        ("sessions", "canonical_sessions"),
        ("messages", "canonical_messages"),
        ("runs", "canonical_runs"),
        ("run_states", "observed_run_states"),
        ("presences", "canonical_presences"),
        ("delegations", "canonical_delegations"),
        ("teams", "canonical_teams"),
        ("team_members", "canonical_team_members"),
        ("team_inboxes", "canonical_team_inboxes"),
        ("team_inbox_messages", "canonical_team_inbox_messages"),
        ("task_collections", "canonical_task_collections"),
        ("tasks", "canonical_tasks"),
        ("plans", "canonical_plans"),
        ("artifacts", "canonical_artifacts"),
        ("workflows", "canonical_workflows"),
        ("workflow_members", "canonical_workflow_members"),
        (
            "project_memory_documents",
            "canonical_project_memory_documents",
        ),
        ("persisted_tool_results", "canonical_persisted_tool_results"),
        (
            "interpretation_settings_documents",
            "canonical_interpretation_settings_documents",
        ),
        (
            "effective_interpretation_settings",
            "canonical_effective_interpretation_settings",
        ),
        ("usage_contributions", "usage_contributions"),
        ("usage_sessions", "usage_totals"),
    ];
    let mut entities = Vec::with_capacity(entity_tables.len() + 1);
    entities.push(NamedCount {
        name: "projects".to_string(),
        count: project_count,
    });
    for (name, table) in entity_tables {
        entities.push(NamedCount {
            name: name.to_string(),
            count: count_table(&transaction, table)?,
        });
    }
    let source_stream_states = grouped_counts(
        &transaction,
        "SELECT stream_state, COUNT(*) FROM source_streams GROUP BY stream_state ORDER BY stream_state",
        "read source stream state statistics",
    )?;
    let projection_readiness = grouped_counts(
        &transaction,
        "SELECT readiness, COUNT(*) FROM projection_versions GROUP BY readiness ORDER BY readiness",
        "read projection readiness statistics",
    )?;
    let database_page_count = pragma_u64(&transaction, "page_count")?;
    let database_page_size_bytes = pragma_u64(&transaction, "page_size")?;
    let allocated_database_bytes = database_page_count
        .checked_mul(database_page_size_bytes)
        .ok_or_else(|| EngineError::Sqlite {
            operation: "calculate allocated database bytes",
            detail: "database page count multiplied by page size overflowed u64".to_string(),
        })?;
    let stats = CanonicalStats {
        contract_version: DETAIL_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        schema_version,
        source_instances: count_table(&transaction, "source_instances")?,
        source_streams: count_table(&transaction, "source_streams")?,
        source_objects: count_table(&transaction, "source_objects")?,
        active_source_objects: query_count(
            &transaction,
            "SELECT COUNT(*) FROM source_objects WHERE state = 'active'",
            "count active source objects",
        )?,
        source_record_errors: count_table(&transaction, "source_record_errors")?,
        ingest_commits: query_count(
            &transaction,
            "SELECT COUNT(*) FROM ingest_commits WHERE committed_at IS NOT NULL",
            "count committed ingest commits",
        )?,
        fact_records: count_table(&transaction, "fact_records")?,
        searchable_messages: query_count(
            &transaction,
            "SELECT COUNT(*) FROM canonical_messages WHERE search_text IS NOT NULL AND trim(search_text) <> ''",
            "count searchable canonical messages",
        )?,
        entities,
        source_stream_states,
        projection_readiness,
        database_page_count,
        database_page_size_bytes,
        allocated_database_bytes,
    };
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish canonical statistics snapshot", error))?;
    Ok(stats)
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
        .map_err(|error| query_sqlite_error("validate message session scope", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(
            "message session id does not identify a current session in the requested project"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_page_limit(limit: u32, label: &'static str) -> Result<(), EngineError> {
    if !(1..=MAX_DETAIL_PAGE_LIMIT).contains(&limit) {
        return Err(EngineError::InvalidQuery(format!(
            "{label} page limit must be between 1 and {MAX_DETAIL_PAGE_LIMIT}, got {limit}"
        )));
    }
    Ok(())
}

fn encode_message_cursor(cursor: &MessageCursor) -> Result<String, EngineError> {
    encode_cursor(cursor, "message")
}

fn decode_message_cursor(
    value: &str,
    request: &MessagePageRequest,
) -> Result<MessageCursor, EngineError> {
    let cursor: MessageCursor = decode_cursor(value, "message")?;
    if cursor.version != DETAIL_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported message cursor version {}",
            cursor.version
        )));
    }
    if cursor.project_id != request.project_id || cursor.session_id != request.session_id {
        return Err(EngineError::InvalidQuery(
            "message cursor does not belong to this query scope".to_string(),
        ));
    }
    if cursor.untimed_rank > 1 || cursor.sort_time.len() > MAX_DETAIL_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "message cursor order key is outside the supported bounds".to_string(),
        ));
    }
    let key = URL_SAFE_NO_PAD.decode(&cursor.message_key).map_err(|_| {
        EngineError::InvalidQuery("message cursor entity key is malformed".to_string())
    })?;
    if key.is_empty() || key.len() > MAX_DETAIL_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "message cursor entity key is outside the supported bounds".to_string(),
        ));
    }
    Ok(cursor)
}

fn cursor_message_key(cursor: Option<&MessageCursor>) -> Result<Vec<u8>, EngineError> {
    cursor
        .map(|cursor| {
            URL_SAFE_NO_PAD.decode(&cursor.message_key).map_err(|_| {
                EngineError::InvalidQuery("message cursor entity key is malformed".to_string())
            })
        })
        .transpose()
        .map(|key| key.unwrap_or_default())
}

fn encode_source_cursor(cursor: &SourceCursor) -> Result<String, EngineError> {
    encode_cursor(cursor, "source")
}

fn decode_source_cursor(value: &str) -> Result<SourceCursor, EngineError> {
    let cursor: SourceCursor = decode_cursor(value, "source")?;
    if cursor.version != DETAIL_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported source cursor version {}",
            cursor.version
        )));
    }
    if cursor.adapter_id.is_empty() || cursor.adapter_id.len() > MAX_DETAIL_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "source cursor adapter id is outside the supported bounds".to_string(),
        ));
    }
    Ok(cursor)
}

fn encode_cursor<T: Serialize>(cursor: &T, label: &'static str) -> Result<String, EngineError> {
    let json = serde_json::to_vec(cursor).map_err(|error| {
        EngineError::InvalidQuery(format!("could not encode {label} cursor: {error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode_cursor<T: for<'de> Deserialize<'de>>(
    value: &str,
    label: &'static str,
) -> Result<T, EngineError> {
    if value.is_empty() || value.len() > MAX_DETAIL_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(format!(
            "{label} cursor is empty or exceeds the supported bound"
        )));
    }
    let json = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| EngineError::InvalidQuery(format!("{label} cursor is not valid base64url")))?;
    serde_json::from_slice(&json)
        .map_err(|_| EngineError::InvalidQuery(format!("{label} cursor payload is malformed")))
}

fn validate_cursor_watermark(
    cursor_watermark: Option<u64>,
    current_watermark: u64,
    label: &'static str,
) -> Result<(), EngineError> {
    if let Some(cursor_watermark) = cursor_watermark {
        if cursor_watermark != current_watermark {
            return Err(EngineError::InvalidQuery(format!(
                "{label} cursor expired at commit {cursor_watermark}; current commit is {current_watermark}"
            )));
        }
    }
    Ok(())
}

fn grouped_counts(
    transaction: &Transaction<'_>,
    sql: &str,
    operation: &'static str,
) -> Result<Vec<NamedCount>, EngineError> {
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| query_sqlite_error(operation, error))?;
    let rows = statement
        .query_map([], |row| {
            Ok(NamedCount {
                name: row.get(0)?,
                count: sql_u64(row.get(1)?, 1)?,
            })
        })
        .map_err(|error| query_sqlite_error(operation, error))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error(operation, error))
}

fn count_table(transaction: &Transaction<'_>, table: &str) -> Result<u64, EngineError> {
    query_count(
        transaction,
        &format!("SELECT COUNT(*) FROM {table}"),
        "count canonical statistics table",
    )
}

fn query_count(
    transaction: &Transaction<'_>,
    sql: &str,
    operation: &'static str,
) -> Result<u64, EngineError> {
    let count = transaction
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .map_err(|error| query_sqlite_error(operation, error))?;
    decode_nonnegative_u64(count, "statistics count")
}

fn pragma_u64(transaction: &Transaction<'_>, pragma: &str) -> Result<u64, EngineError> {
    let value = transaction
        .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))
        .map_err(|error| query_sqlite_error("read database size pragma", error))?;
    decode_nonnegative_u64(value, "database size pragma")
}

fn query_get<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    index: usize,
    operation: &'static str,
) -> Result<T, EngineError> {
    row.get(index)
        .map_err(|error| query_sqlite_error(operation, error))
}

fn sql_u64(value: i64, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn decode_nonnegative_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode detail integer",
        detail: format!("{field} was negative: {value}"),
    })
}

fn decode_nonnegative_u32(value: i64, field: &'static str) -> Result<u32, EngineError> {
    u32::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode detail integer",
        detail: format!("{field} was outside u32: {value}"),
    })
}

fn decode_nonnegative_u8(value: i64, field: &'static str) -> Result<u8, EngineError> {
    u8::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode detail integer",
        detail: format!("{field} was outside u8: {value}"),
    })
}

fn decode_optional_u64(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<u64>, EngineError> {
    value
        .map(|value| decode_nonnegative_u64(value, field))
        .transpose()
}

fn to_query_i64(value: u64, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| {
        EngineError::InvalidQuery(format!("{field} is outside SQLite's signed integer range"))
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
    use super::*;
    use crate::core::schema;
    use rusqlite::params;

    fn insert_fact(
        connection: &Connection,
        fact_id: &[u8],
        kind: &str,
        entity_key: &[u8],
        ordinal: i64,
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
                ) VALUES (?1, ?2, ?3, 1, 1, 1, 1, ?4, ?5, ?6, ?7, ?8, ?9, 1)
                "#,
                params![
                    fact_id,
                    kind,
                    entity_key,
                    format!("start-{ordinal}").as_bytes(),
                    format!("end-{ordinal}").as_bytes(),
                    [ordinal as u8; 32].as_slice(),
                    ordinal,
                    1_786_507_200_000_i64 + ordinal,
                    b"{}".as_slice(),
                ],
            )
            .unwrap();
    }

    fn insert_message(
        connection: &Connection,
        key: &[u8],
        fact_id: &[u8],
        native_id: &str,
        source_time: Option<&str>,
        search_text: Option<&str>,
        ordinal: i64,
    ) {
        insert_fact(connection, fact_id, "message", key, ordinal);
        let content = format!(r#"[{{"kind":"text","text":"{native_id}"}}]"#);
        let raw = format!(
            r#"{{"type":"user","uuid":"{native_id}","message":{{"role":"user","content":"{native_id}"}}}}"#
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
                ) VALUES (?1, ?2, ?3, ?4, 'user', 'user', ?5, ?6,
                          CASE WHEN ?6 IS NULL THEN NULL ELSE 'native_exact' END,
                          NULL, NULL, ?7, ?8, ?9, 1, 1, ?10, ?11, 1)
                "#,
                params![
                    key,
                    b"session".as_slice(),
                    b"run".as_slice(),
                    native_id,
                    content.as_bytes(),
                    source_time,
                    search_text,
                    raw.as_bytes(),
                    fact_id,
                    format!("message-start-{ordinal}").as_bytes(),
                    format!("message-end-{ordinal}").as_bytes(),
                ],
            )
            .unwrap();
    }

    fn seeded_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_instances VALUES (1, 'fixture', ?1, 'Fixture', '1.2.3', 1, '[\"fixture-v1\"]', '[{\"id\":\"history\",\"support_level\":\"native\",\"granularity\":\"message\",\"availability\":\"live\",\"notes\":null}]', 10, 20)",
                [b"fixture-root".as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_instances VALUES (2, 'alpha', ?1, 'Alpha', '2.0.0', 2, '[]', '[]', 11, 21)",
                [b"alpha-root".as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_streams VALUES (1, 1, 'transcripts', 'append_delimited', 'fixture', 'available', 20, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_objects (
                    source_object_id, source_stream_id, object_key, display_path,
                    generation, committed_cursor, size_bytes, mtime_ns,
                    decoder_contract_version, last_commit_seq, state
                ) VALUES (1, 1, ?1, '/fixture/session.jsonl', 1, ?2, 100, 20, 1, 1, 'active')",
                params![b"object".as_slice(), b"cursor".as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (1, 1, 'fixture', 1, 2, 4)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (2, 2, 'catalog', 3, 4, 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO projection_versions VALUES ('messages', ?1, 1, 1, 'ready', 1, 5, NULL)",
                [b"fixture-scope".as_slice()],
            )
            .unwrap();

        insert_fact(&connection, b"session-fact", "session", b"session", 1);
        connection
            .execute(
                r#"
                INSERT INTO canonical_sessions (
                    session_key, project_key, native_session_id,
                    native_project_key, cwd, git_branch, first_prompt,
                    ai_title, custom_title, source_time, source_time_quality,
                    fact_id, source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES (?1, ?2, 'native-session', 'native-project',
                          '/fixture/project', 'main', 'hello', 'AI title',
                          'Custom title', '2026-08-12T01:00:00.000Z',
                          'native_exact', ?3, 1, 1, ?4, 1)
                "#,
                params![
                    b"session".as_slice(),
                    b"project".as_slice(),
                    b"session-fact".as_slice(),
                    b"session-cursor".as_slice(),
                ],
            )
            .unwrap();
        insert_message(
            &connection,
            b"message-middle",
            b"message-middle-fact",
            "middle",
            Some("2026-08-12T02:00:00.000Z"),
            Some("middle searchable"),
            2,
        );
        insert_message(
            &connection,
            b"message-first",
            b"message-first-fact",
            "first",
            Some("2026-08-12T01:30:00.000Z"),
            Some("first searchable"),
            3,
        );
        insert_message(
            &connection,
            b"message-untimed",
            b"message-untimed-fact",
            "untimed",
            None,
            None,
            4,
        );
        connection
    }

    #[test]
    fn session_detail_and_message_pages_are_typed_bounded_and_deterministic() {
        let connection = seeded_connection();
        let project_id = encode_entity_id(PROJECT_ID_PREFIX, b"project");
        let session_id = encode_entity_id(SESSION_ID_PREFIX, b"session");

        let details = read_session_details(
            &connection,
            &SessionDetailsRequest {
                session_id: session_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(details.contract_version, DETAIL_QUERY_CONTRACT_VERSION);
        assert_eq!(details.at_commit_seq, 2);
        let session = details.session.unwrap();
        assert_eq!(session.project_id, project_id);
        assert_eq!(session.adapter_id, "fixture");
        assert_eq!(session.native_session_id, "native-session");
        assert_eq!(session.message_count, 3);
        assert_eq!(session.run_count, 0);
        assert!(session.index.is_none());
        assert!(session.decisive_fact_id.starts_with(FACT_ID_PREFIX));

        let unknown = read_session_details(
            &connection,
            &SessionDetailsRequest {
                session_id: encode_entity_id(SESSION_ID_PREFIX, b"unknown"),
            },
        )
        .unwrap();
        assert!(unknown.session.is_none());

        let first = read_message_page(
            &connection,
            &MessagePageRequest {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                cursor: None,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.items[0].native_message_id.as_deref(), Some("first"));
        assert_eq!(first.items[1].native_message_id.as_deref(), Some("middle"));
        assert_eq!(first.items[0].content[0]["text"], "first");
        assert_eq!(first.items[0].native_payload["uuid"], "first");
        assert!(first.items[0].message_id.starts_with(MESSAGE_ID_PREFIX));
        assert!(first.payload_bytes > 0);
        assert_eq!(first.payload_byte_limit, MAX_MESSAGE_PAGE_PAYLOAD_BYTES);

        let second = read_message_page(
            &connection,
            &MessagePageRequest {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                cursor: first.next_cursor,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(
            second.items[0].native_message_id.as_deref(),
            Some("untimed")
        );
        assert!(second.next_cursor.is_none());

        assert!(matches!(
            read_message_page(
                &connection,
                &MessagePageRequest {
                    project_id: encode_entity_id(PROJECT_ID_PREFIX, b"other"),
                    session_id,
                    cursor: None,
                    limit: 1,
                },
            ),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn cursors_are_scope_and_commit_watermark_bound() {
        let connection = seeded_connection();
        let project_id = encode_entity_id(PROJECT_ID_PREFIX, b"project");
        let session_id = encode_entity_id(SESSION_ID_PREFIX, b"session");
        let message_page = read_message_page(
            &connection,
            &MessagePageRequest {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        let message_cursor = message_page.next_cursor.unwrap();
        assert!(matches!(
            validate_message_page(&MessagePageRequest {
                project_id: project_id.clone(),
                session_id: encode_entity_id(SESSION_ID_PREFIX, b"other"),
                cursor: Some(message_cursor.clone()),
                limit: 1,
            }),
            Err(EngineError::InvalidQuery(_))
        ));

        let source_page = read_source_page(
            &connection,
            &SourcePageRequest {
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        let source_cursor = source_page.next_cursor.unwrap();
        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (3, 1, 'later', 6, 7, 0)",
                [],
            )
            .unwrap();
        assert!(matches!(
            read_source_page(
                &connection,
                &SourcePageRequest {
                    cursor: Some(source_cursor),
                    limit: 1,
                },
            ),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            read_message_page(
                &connection,
                &MessagePageRequest {
                    project_id,
                    session_id,
                    cursor: Some(message_cursor),
                    limit: 1,
                },
            ),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            validate_source_page(&SourcePageRequest {
                cursor: None,
                limit: 0,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn source_inventory_and_canonical_stats_exclude_compatibility_tables() {
        let connection = seeded_connection();
        connection
            .execute(
                "INSERT INTO projects (slug, original_path, source_id) VALUES ('legacy', '/legacy', 'legacy')",
                [],
            )
            .unwrap();

        let first = read_source_page(
            &connection,
            &SourcePageRequest {
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].adapter_id, "alpha");
        assert!(first.items[0].source_id.starts_with(SOURCE_ID_PREFIX));
        let second = read_source_page(
            &connection,
            &SourcePageRequest {
                cursor: first.next_cursor,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(second.items[0].adapter_id, "fixture");
        assert_eq!(second.items[0].adapter_version, "1.2.3");
        assert_eq!(second.items[0].source_schema_versions, ["fixture-v1"]);
        assert_eq!(second.items[0].capabilities.len(), 1);
        assert_eq!(second.items[0].capabilities[0].id, "history");
        assert_eq!(second.items[0].capabilities[0].support_level, "native");
        assert_eq!(second.items[0].capabilities[0].granularity, "message");
        assert_eq!(second.items[0].capabilities[0].availability, "live");
        assert_eq!(second.items[0].stream_count, 1);
        assert_eq!(second.items[0].active_object_count, 1);
        assert_eq!(second.items[0].fact_count, 4);
        assert_eq!(second.items[0].commit_count, 1);

        let stats = read_canonical_stats(&connection).unwrap();
        assert_eq!(stats.at_commit_seq, 2);
        assert_eq!(stats.schema_version, schema::SCHEMA_VERSION);
        assert_eq!(stats.source_instances, 2);
        assert_eq!(stats.source_streams, 1);
        assert_eq!(stats.fact_records, 4);
        assert_eq!(stats.searchable_messages, 2);
        assert_eq!(
            stats
                .entities
                .iter()
                .find(|count| count.name == "projects")
                .unwrap()
                .count,
            1,
            "the compatibility-only project must not affect canonical stats"
        );
        assert_eq!(stats.source_stream_states[0].name, "available");
        assert_eq!(stats.projection_readiness[0].name, "ready");
        assert_eq!(
            stats.allocated_database_bytes,
            stats.database_page_count * stats.database_page_size_bytes
        );
    }

    #[test]
    fn source_statistics_index_is_installed() {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        let names = connection
            .prepare("PRAGMA index_list('fact_records')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(names
            .iter()
            .any(|name| name == "idx_fact_records_source_instance"));

        let plan = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT source_instance_id, COUNT(*)
                 FROM fact_records
                 GROUP BY source_instance_id",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|step| {
                step.contains("USING COVERING INDEX idx_fact_records_source_instance")
            }),
            "source statistics must use the covering source-instance index: {plan:?}"
        );
    }
}
