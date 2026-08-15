//! Read-only RFC 011 capability-detail query pack.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use super::query_identity::{
    decode_entity_id, encode_entity_id, ARTIFACT_ID_PREFIX, FACT_ID_PREFIX,
    MEMORY_DOCUMENT_ID_PREFIX, MESSAGE_ID_PREFIX, PLAN_ID_PREFIX, PROJECT_ID_PREFIX, RUN_ID_PREFIX,
    SESSION_ID_PREFIX, TASK_COLLECTION_ID_PREFIX, TASK_ID_PREFIX, TEAM_ID_PREFIX,
    TOOL_RESULT_ID_PREFIX,
};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const CAPABILITY_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_CAPABILITY_PAGE_LIMIT: u32 = 50;
const MAX_CAPABILITY_PAGE_LIMIT: u32 = 200;
const MAX_CAPABILITY_CURSOR_BYTES: usize = 32 * 1024;
/// Maximum encoded or textual content returned by one capability page.
pub const MAX_CAPABILITY_PAGE_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDocumentPageRequest {
    pub project_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDocumentPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub items: Vec<MemoryDocument>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDocument {
    pub document_id: String,
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_project_key: String,
    pub native_document_path: String,
    pub title: String,
    pub content: String,
    pub size_bytes: u64,
    pub is_index: bool,
    pub resolution_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_document_count: u64,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCollectionPageRequest {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub team_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCollectionPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub team_id: Option<String>,
    pub items: Vec<TaskCollectionSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCollectionSummary {
    pub collection_id: String,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub team_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_collection_id: String,
    pub native_owner_id: Option<String>,
    pub collection_kind: String,
    pub native_collection_kind: String,
    pub resolution_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_metadata_count: u64,
    pub complete_snapshot_count: u64,
    pub item_document_count: u64,
    pub item_count: u64,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPageRequest {
    pub collection_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub collection_id: String,
    pub items: Vec<TaskDetail>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDetail {
    pub task_id: String,
    pub collection_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub item_ordinal: u64,
    pub native_task_id: Option<String>,
    pub subject: String,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub native_owner: Option<String>,
    pub task_status: String,
    pub native_status: String,
    pub blocks: Vec<String>,
    pub blocked_by: Vec<String>,
    pub resolution_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_item_count: u64,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanPageRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub items: Vec<PlanDetail>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDetail {
    pub plan_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_plan_id: String,
    pub title: String,
    pub content: String,
    pub size_bytes: u64,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub resolution_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_plan_count: u64,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultPageRequest {
    pub project_id: String,
    pub session_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<ToolResultDetail>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultDetail {
    pub result_id: String,
    pub project_id: String,
    pub session_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_project_key: String,
    pub native_session_id: String,
    pub native_tool_use_id: String,
    pub native_document_path: String,
    pub content: String,
    pub size_bytes: u64,
    pub resolution_status: String,
    pub correlation_status: String,
    pub tool_call_message_id: Option<String>,
    pub tool_result_message_id: Option<String>,
    pub decisive_fact_id: String,
    pub assertion_count: u64,
    pub competing_result_count: u64,
    pub tool_call_match_count: u64,
    pub tool_result_match_count: u64,
    pub join_conflict: bool,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPageRequest {
    pub session_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub session_id: String,
    pub items: Vec<ArtifactDetail>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDetail {
    pub artifact_id: String,
    pub session_id: String,
    pub project_id: Option<String>,
    pub native_artifact_id: Option<String>,
    pub native_file_hash: Option<String>,
    pub version: u64,
    pub tracking_path: Option<String>,
    pub real_parent_dir: Option<String>,
    pub backup_time: Option<String>,
    pub backup_time_quality: Option<String>,
    pub capture_status: String,
    pub content_base64: Option<String>,
    pub size_bytes: Option<u64>,
    pub content_digest_base64url: Option<String>,
    pub content_status: String,
    pub resolution_status: String,
    pub metadata_fact_id: Option<String>,
    pub content_fact_id: Option<String>,
    pub metadata_adapter_id: Option<String>,
    pub metadata_source_instance_id: Option<u64>,
    pub metadata_observed_at_unix_ms: Option<i64>,
    pub metadata_source_object_id: Option<u64>,
    pub metadata_source_generation: Option<u64>,
    pub content_adapter_id: Option<String>,
    pub content_source_instance_id: Option<u64>,
    pub content_observed_at_unix_ms: Option<i64>,
    pub content_source_object_id: Option<u64>,
    pub content_source_generation: Option<u64>,
    pub metadata_assertion_count: u64,
    pub competing_metadata_count: u64,
    pub content_assertion_count: u64,
    pub competing_content_count: u64,
    pub join_conflict: bool,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CapabilityCursorKind {
    MemoryDocuments,
    TaskCollections,
    Tasks,
    Plans,
    ToolResults,
    Artifacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CapabilityCursor {
    version: u32,
    kind: CapabilityCursorKind,
    at_commit_seq: u64,
    scope: Vec<String>,
    order_rank: u64,
    order_text: String,
    entity_key: String,
}

#[derive(Debug)]
struct OrderedRow<T> {
    item: T,
    entity_key: Vec<u8>,
    order_rank: u64,
    order_text: String,
    payload_bytes: u64,
}

#[derive(Debug)]
pub(super) struct TaskCollectionScopeKeys {
    session_key: Option<Vec<u8>>,
    run_key: Option<Vec<u8>>,
    team_key: Option<Vec<u8>>,
}

pub(super) fn validate_memory_document_page(
    request: &MemoryDocumentPageRequest,
) -> Result<Vec<u8>, EngineError> {
    validate_page_limit(request.limit, "memory document")?;
    let project_key = decode_entity_id(
        &request.project_id,
        PROJECT_ID_PREFIX,
        "memory document project id",
    )?;
    validate_request_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::MemoryDocuments,
        std::slice::from_ref(&request.project_id),
    )?;
    Ok(project_key)
}

pub(super) fn validate_task_collection_page(
    request: &TaskCollectionPageRequest,
) -> Result<TaskCollectionScopeKeys, EngineError> {
    validate_page_limit(request.limit, "task collection")?;
    let scope_count = [
        request.session_id.is_some(),
        request.run_id.is_some(),
        request.team_id.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if scope_count > 1 {
        return Err(EngineError::InvalidQuery(
            "task collection query accepts at most one of sessionId, runId, or teamId".to_string(),
        ));
    }
    let keys = TaskCollectionScopeKeys {
        session_key: request
            .session_id
            .as_deref()
            .map(|value| decode_entity_id(value, SESSION_ID_PREFIX, "task collection session id"))
            .transpose()?,
        run_key: request
            .run_id
            .as_deref()
            .map(|value| decode_entity_id(value, RUN_ID_PREFIX, "task collection run id"))
            .transpose()?,
        team_key: request
            .team_id
            .as_deref()
            .map(|value| decode_entity_id(value, TEAM_ID_PREFIX, "task collection team id"))
            .transpose()?,
    };
    validate_request_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::TaskCollections,
        &task_collection_scope(request),
    )?;
    Ok(keys)
}

pub(super) fn validate_task_page(request: &TaskPageRequest) -> Result<Vec<u8>, EngineError> {
    validate_page_limit(request.limit, "task")?;
    let collection_key = decode_entity_id(
        &request.collection_id,
        TASK_COLLECTION_ID_PREFIX,
        "task collection id",
    )?;
    validate_request_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::Tasks,
        std::slice::from_ref(&request.collection_id),
    )?;
    Ok(collection_key)
}

pub(super) fn validate_plan_page(request: &PlanPageRequest) -> Result<(), EngineError> {
    validate_page_limit(request.limit, "plan")?;
    validate_request_cursor(request.cursor.as_deref(), CapabilityCursorKind::Plans, &[])
}

pub(super) fn validate_tool_result_page(
    request: &ToolResultPageRequest,
) -> Result<(Vec<u8>, Vec<u8>), EngineError> {
    validate_page_limit(request.limit, "tool result")?;
    let project_key = decode_entity_id(
        &request.project_id,
        PROJECT_ID_PREFIX,
        "tool result project id",
    )?;
    let session_key = decode_entity_id(
        &request.session_id,
        SESSION_ID_PREFIX,
        "tool result session id",
    )?;
    validate_request_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::ToolResults,
        &[request.project_id.clone(), request.session_id.clone()],
    )?;
    Ok((project_key, session_key))
}

pub(super) fn validate_artifact_page(
    request: &ArtifactPageRequest,
) -> Result<Vec<u8>, EngineError> {
    validate_page_limit(request.limit, "artifact")?;
    let session_key = decode_entity_id(
        &request.session_id,
        SESSION_ID_PREFIX,
        "artifact session id",
    )?;
    validate_request_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::Artifacts,
        std::slice::from_ref(&request.session_id),
    )?;
    Ok(session_key)
}

pub(super) fn read_memory_document_page(
    connection: &Connection,
    request: &MemoryDocumentPageRequest,
) -> Result<MemoryDocumentPage, EngineError> {
    let project_key = validate_memory_document_page(request)?;
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::MemoryDocuments,
        std::slice::from_ref(&request.project_id),
    )?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_rank = cursor.as_ref().map_or(0, |value| value.order_rank);
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());
    let transaction = begin_snapshot(connection, "memory document")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark, "memory document")?;
    let mut statement = transaction
        .prepare(
            r#"
            WITH memory_rows AS (
                SELECT md.document_key, md.project_key, md.native_project_key,
                       md.native_document_path, md.title, md.content,
                       md.size_bytes, md.is_index, md.resolution_status,
                       md.decisive_fact_id, md.assertion_count,
                       md.competing_document_count, md.last_commit_seq,
                       fr.observed_at, fr.source_object_id,
                       fr.source_generation, fr.source_instance_id,
                       si.adapter_id,
                       CASE WHEN md.is_index != 0 THEN 0 ELSE 1 END AS order_rank
                FROM canonical_project_memory_documents md
                JOIN fact_records fr ON fr.fact_id = md.decisive_fact_id
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
                WHERE md.project_key = ?1
            )
            SELECT document_key, project_key, native_project_key,
                   native_document_path, title, content, size_bytes, is_index,
                   resolution_status, decisive_fact_id, assertion_count,
                   competing_document_count, last_commit_seq, observed_at,
                   source_object_id, source_generation, source_instance_id,
                   adapter_id, order_rank
            FROM memory_rows
            WHERE (?2 = 0)
               OR order_rank > ?3
               OR (order_rank = ?3 AND native_document_path > ?4)
               OR (order_rank = ?3 AND native_document_path = ?4
                   AND document_key > ?5)
            ORDER BY order_rank, native_document_path, document_key
            LIMIT ?6
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare memory document page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            project_key,
            i64::from(cursor.is_some()),
            to_query_i64(cursor_rank, "memory cursor order rank")?,
            cursor_text,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute memory document page", error))?;
    let (items, payload_bytes, has_more) = collect_bounded_rows(
        &mut rows,
        request.limit,
        decode_memory_document_row,
        "memory document",
    )?;
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "memory document")?;
    let next_cursor = page_cursor(
        has_more,
        items.last(),
        CapabilityCursorKind::MemoryDocuments,
        watermark,
        vec![request.project_id.clone()],
    )?;
    Ok(MemoryDocumentPage {
        contract_version: CAPABILITY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        items: items.into_iter().map(|row| row.item).collect(),
        payload_bytes,
        payload_byte_limit: MAX_CAPABILITY_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

pub(super) fn read_task_collection_page(
    connection: &Connection,
    request: &TaskCollectionPageRequest,
) -> Result<TaskCollectionPage, EngineError> {
    let keys = validate_task_collection_page(request)?;
    let scope = task_collection_scope(request);
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::TaskCollections,
        &scope,
    )?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());
    let transaction = begin_snapshot(connection, "task collection")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark, "task collection")?;
    let (scope_column, scope_key) = if let Some(key) = keys.session_key {
        (Some("tc.session_key"), Some(key))
    } else if let Some(key) = keys.run_key {
        (Some("tc.run_key"), Some(key))
    } else if let Some(key) = keys.team_key {
        (Some("tc.team_key"), Some(key))
    } else {
        (None, None)
    };
    let mut arguments = Vec::new();
    let scope_predicate = if let (Some(column), Some(key)) = (scope_column, scope_key) {
        arguments.push(Value::Blob(key));
        format!("{column} = ?{}", arguments.len())
    } else {
        "1 = 1".to_string()
    };
    arguments.push(Value::Integer(i64::from(cursor.is_some())));
    let cursor_present_parameter = arguments.len();
    arguments.push(Value::Text(cursor_text.to_string()));
    let cursor_text_parameter = arguments.len();
    arguments.push(Value::Blob(cursor_key));
    let cursor_key_parameter = arguments.len();
    arguments.push(Value::Integer(i64::from(request.limit) + 1));
    let limit_parameter = arguments.len();
    let sql = format!(
        r#"
        SELECT tc.collection_key, cs.project_key, tc.session_key,
               tc.run_key, tc.team_key, tc.native_collection_id,
               tc.native_owner_id, tc.collection_kind,
               tc.native_collection_kind, tc.resolution_status,
               tc.decisive_fact_id, tc.assertion_count,
               tc.competing_metadata_count, tc.complete_snapshot_count,
               tc.item_document_count, tc.item_count, tc.last_commit_seq,
               fr.observed_at, fr.source_object_id,
               fr.source_generation, fr.source_instance_id, si.adapter_id
        FROM canonical_task_collections tc
        JOIN fact_records fr ON fr.fact_id = tc.decisive_fact_id
        JOIN source_instances si
          ON si.source_instance_id = fr.source_instance_id
        LEFT JOIN canonical_sessions cs ON cs.session_key = tc.session_key
        WHERE {scope_predicate}
          AND (
              ?{cursor_present_parameter} = 0
              OR tc.native_collection_id > ?{cursor_text_parameter}
              OR (tc.native_collection_id = ?{cursor_text_parameter}
                  AND tc.collection_key > ?{cursor_key_parameter})
          )
        ORDER BY tc.native_collection_id, tc.collection_key
        LIMIT ?{limit_parameter}
        "#,
    );
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| query_sqlite_error("prepare task collection page", error))?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(arguments.iter()))
        .map_err(|error| query_sqlite_error("execute task collection page", error))?;
    let mut items = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance task collection page", error))?
    {
        items.push(decode_task_collection_row(row)?);
    }
    let has_more = items.len() > request.limit as usize;
    if has_more {
        items.pop();
    }
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "task collection")?;
    let next_cursor = page_cursor(
        has_more,
        items.last(),
        CapabilityCursorKind::TaskCollections,
        watermark,
        scope,
    )?;
    Ok(TaskCollectionPage {
        contract_version: CAPABILITY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        team_id: request.team_id.clone(),
        items: items.into_iter().map(|row| row.item).collect(),
        next_cursor,
    })
}

pub(super) fn read_task_page(
    connection: &Connection,
    request: &TaskPageRequest,
) -> Result<TaskPage, EngineError> {
    let collection_key = validate_task_page(request)?;
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::Tasks,
        std::slice::from_ref(&request.collection_id),
    )?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_rank = cursor.as_ref().map_or(0, |value| value.order_rank);
    let transaction = begin_snapshot(connection, "task")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark, "task")?;
    let mut statement = transaction
        .prepare(
            r#"
            SELECT ct.task_key, ct.collection_key, ct.item_ordinal,
                   ct.native_task_id, ct.subject, ct.description,
                   ct.active_form, ct.native_owner, ct.task_status,
                   ct.native_status, ct.blocks_json, ct.blocked_by_json,
                   ct.resolution_status, ct.decisive_fact_id,
                   ct.assertion_count, ct.competing_item_count,
                   ct.last_commit_seq, fr.observed_at, fr.source_object_id,
                   fr.source_generation, fr.source_instance_id, si.adapter_id
            FROM canonical_tasks ct
            JOIN fact_records fr ON fr.fact_id = ct.decisive_fact_id
            JOIN source_instances si
              ON si.source_instance_id = fr.source_instance_id
            WHERE ct.collection_key = ?1
              AND (
                  ?2 = 0
                  OR ct.item_ordinal > ?3
                  OR (ct.item_ordinal = ?3 AND ct.task_key > ?4)
              )
            ORDER BY ct.item_ordinal, ct.task_key
            LIMIT ?5
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare task page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            collection_key,
            i64::from(cursor.is_some()),
            to_query_i64(cursor_rank, "task cursor item ordinal")?,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute task page", error))?;
    let (items, payload_bytes, has_more) =
        collect_bounded_rows(&mut rows, request.limit, decode_task_row, "task")?;
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "task")?;
    let next_cursor = page_cursor(
        has_more,
        items.last(),
        CapabilityCursorKind::Tasks,
        watermark,
        vec![request.collection_id.clone()],
    )?;
    Ok(TaskPage {
        contract_version: CAPABILITY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        collection_id: request.collection_id.clone(),
        items: items.into_iter().map(|row| row.item).collect(),
        payload_bytes,
        payload_byte_limit: MAX_CAPABILITY_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

pub(super) fn read_plan_page(
    connection: &Connection,
    request: &PlanPageRequest,
) -> Result<PlanPage, EngineError> {
    validate_plan_page(request)?;
    let cursor =
        decode_optional_cursor(request.cursor.as_deref(), CapabilityCursorKind::Plans, &[])?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());
    let transaction = begin_snapshot(connection, "plan")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark, "plan")?;
    let mut statement = transaction
        .prepare(
            r#"
            SELECT cp.plan_key, cp.native_plan_id, cp.title, cp.content,
                   cp.size_bytes, cp.source_time, cp.source_time_quality,
                   cp.resolution_status, cp.decisive_fact_id,
                   cp.assertion_count, cp.competing_plan_count,
                   cp.last_commit_seq, fr.observed_at, fr.source_object_id,
                   fr.source_generation, fr.source_instance_id, si.adapter_id
            FROM canonical_plans cp
            JOIN fact_records fr ON fr.fact_id = cp.decisive_fact_id
            JOIN source_instances si
              ON si.source_instance_id = fr.source_instance_id
            WHERE (?1 = 0)
               OR cp.native_plan_id > ?2
               OR (cp.native_plan_id = ?2 AND cp.plan_key > ?3)
            ORDER BY cp.native_plan_id, cp.plan_key
            LIMIT ?4
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare plan page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            i64::from(cursor.is_some()),
            cursor_text,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute plan page", error))?;
    let (items, payload_bytes, has_more) =
        collect_bounded_rows(&mut rows, request.limit, decode_plan_row, "plan")?;
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "plan")?;
    let next_cursor = page_cursor(
        has_more,
        items.last(),
        CapabilityCursorKind::Plans,
        watermark,
        Vec::new(),
    )?;
    Ok(PlanPage {
        contract_version: CAPABILITY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        items: items.into_iter().map(|row| row.item).collect(),
        payload_bytes,
        payload_byte_limit: MAX_CAPABILITY_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

pub(super) fn read_tool_result_page(
    connection: &Connection,
    request: &ToolResultPageRequest,
) -> Result<ToolResultPage, EngineError> {
    let (project_key, session_key) = validate_tool_result_page(request)?;
    let scope = vec![request.project_id.clone(), request.session_id.clone()];
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::ToolResults,
        &scope,
    )?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());
    let transaction = begin_snapshot(connection, "tool result")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark, "tool result")?;
    require_session_membership(&transaction, &project_key, &session_key, "tool result")?;
    let mut statement = transaction
        .prepare(
            r#"
            SELECT tr.result_key, tr.project_key, tr.session_key,
                   tr.native_project_key, tr.native_session_id,
                   tr.native_tool_use_id, tr.native_document_path,
                   decisive.content,
                   tr.size_bytes, tr.resolution_status,
                   tr.correlation_status, tr.tool_call_message_key,
                   tr.tool_result_message_key, tr.decisive_fact_id,
                   tr.assertion_count, tr.competing_result_count,
                   tr.tool_call_match_count, tr.tool_result_match_count,
                   tr.join_conflict, tr.last_commit_seq, fr.observed_at,
                   fr.source_object_id, fr.source_generation,
                   fr.source_instance_id, si.adapter_id
            FROM canonical_persisted_tool_results tr
            JOIN persisted_tool_result_assertions decisive
              ON decisive.fact_id = tr.decisive_fact_id
            JOIN fact_records fr ON fr.fact_id = tr.decisive_fact_id
            JOIN source_instances si
              ON si.source_instance_id = fr.source_instance_id
            WHERE tr.project_key = ?1 AND tr.session_key = ?2
              AND (
                  ?3 = 0
                  OR tr.native_tool_use_id > ?4
                  OR (tr.native_tool_use_id = ?4 AND tr.result_key > ?5)
              )
            ORDER BY tr.native_tool_use_id, tr.result_key
            LIMIT ?6
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare tool result page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            project_key,
            session_key,
            i64::from(cursor.is_some()),
            cursor_text,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute tool result page", error))?;
    let (items, payload_bytes, has_more) = collect_bounded_rows(
        &mut rows,
        request.limit,
        decode_tool_result_row,
        "tool result",
    )?;
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "tool result")?;
    let next_cursor = page_cursor(
        has_more,
        items.last(),
        CapabilityCursorKind::ToolResults,
        watermark,
        scope,
    )?;
    Ok(ToolResultPage {
        contract_version: CAPABILITY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        items: items.into_iter().map(|row| row.item).collect(),
        payload_bytes,
        payload_byte_limit: MAX_CAPABILITY_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

pub(super) fn read_artifact_page(
    connection: &Connection,
    request: &ArtifactPageRequest,
) -> Result<ArtifactPage, EngineError> {
    let session_key = validate_artifact_page(request)?;
    let scope = vec![request.session_id.clone()];
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        CapabilityCursorKind::Artifacts,
        &scope,
    )?;
    let cursor_key = cursor_entity_key(cursor.as_ref())?;
    let cursor_rank = cursor.as_ref().map_or(0, |value| value.order_rank);
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());
    let transaction = begin_snapshot(connection, "artifact")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark, "artifact")?;
    let mut statement = transaction
        .prepare(
            r#"
            WITH artifact_rows AS (
                SELECT ca.artifact_key, ca.session_key, cs.project_key,
                       ca.native_artifact_id, ca.native_file_hash, ca.version,
                       ca.tracking_path, ca.real_parent_dir, ca.backup_time,
                       ca.backup_time_quality, ca.capture_status,
                       decisive_content.content,
                       ca.size_bytes, ca.content_digest, ca.content_status,
                       ca.resolution_status, ca.decisive_metadata_fact_id,
                       ca.decisive_content_fact_id,
                       ca.metadata_assertion_count,
                       ca.competing_metadata_count,
                       ca.content_assertion_count,
                       ca.competing_content_count, ca.join_conflict,
                       ca.last_commit_seq,
                       mfr.observed_at AS metadata_observed_at,
                       mfr.source_object_id AS metadata_source_object_id,
                       mfr.source_generation AS metadata_source_generation,
                       mfr.source_instance_id AS metadata_source_instance_id,
                       msi.adapter_id AS metadata_adapter_id,
                       cfr.observed_at AS content_observed_at,
                       cfr.source_object_id AS content_source_object_id,
                       cfr.source_generation AS content_source_generation,
                       cfr.source_instance_id AS content_source_instance_id,
                       csi.adapter_id AS content_adapter_id,
                       (ca.backup_time IS NULL)
                           AS order_rank,
                       COALESCE(ca.backup_time, '') AS order_text
                FROM canonical_artifacts ca
                LEFT JOIN fact_records mfr
                  ON mfr.fact_id = ca.decisive_metadata_fact_id
                LEFT JOIN source_instances msi
                  ON msi.source_instance_id = mfr.source_instance_id
                LEFT JOIN fact_records cfr
                  ON cfr.fact_id = ca.decisive_content_fact_id
                LEFT JOIN artifact_content_assertions decisive_content
                  ON decisive_content.fact_id = ca.decisive_content_fact_id
                LEFT JOIN source_instances csi
                  ON csi.source_instance_id = cfr.source_instance_id
                LEFT JOIN canonical_sessions cs
                  ON cs.session_key = ca.session_key
                WHERE ca.session_key = ?1
            )
            SELECT artifact_key, session_key, project_key,
                   native_artifact_id, native_file_hash, version,
                   tracking_path, real_parent_dir, backup_time,
                   backup_time_quality, capture_status, content, size_bytes,
                   content_digest, content_status, resolution_status,
                   decisive_metadata_fact_id, decisive_content_fact_id,
                   metadata_assertion_count, competing_metadata_count,
                   content_assertion_count, competing_content_count,
                   join_conflict, last_commit_seq, metadata_observed_at,
                   metadata_source_object_id, metadata_source_generation,
                   metadata_source_instance_id, metadata_adapter_id,
                   content_observed_at, content_source_object_id,
                   content_source_generation, content_source_instance_id,
                   content_adapter_id, order_rank, order_text
            FROM artifact_rows
            WHERE (?2 = 0)
               OR order_rank > ?3
               OR (order_rank = ?3 AND order_text > ?4)
               OR (order_rank = ?3 AND order_text = ?4
                   AND artifact_key > ?5)
            ORDER BY order_rank, order_text, artifact_key
            LIMIT ?6
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare artifact page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            session_key,
            i64::from(cursor.is_some()),
            to_query_i64(cursor_rank, "artifact cursor order rank")?,
            cursor_text,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute artifact page", error))?;
    let (items, payload_bytes, has_more) =
        collect_bounded_rows(&mut rows, request.limit, decode_artifact_row, "artifact")?;
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "artifact")?;
    let next_cursor = page_cursor(
        has_more,
        items.last(),
        CapabilityCursorKind::Artifacts,
        watermark,
        scope,
    )?;
    Ok(ArtifactPage {
        contract_version: CAPABILITY_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        session_id: request.session_id.clone(),
        items: items.into_iter().map(|row| row.item).collect(),
        payload_bytes,
        payload_byte_limit: MAX_CAPABILITY_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

fn decode_memory_document_row(row: &Row<'_>) -> Result<OrderedRow<MemoryDocument>, EngineError> {
    let document_key: Vec<u8> = query_get(row, 0, "decode memory document key")?;
    let project_key: Vec<u8> = query_get(row, 1, "decode memory project key")?;
    let content: String = query_get(row, 5, "decode memory content")?;
    let fact_id: Vec<u8> = query_get(row, 9, "decode memory fact id")?;
    let order_rank = decode_nonnegative_u64(
        query_get(row, 18, "decode memory order rank")?,
        "memory order rank",
    )?;
    let order_text: String = query_get(row, 3, "decode memory document order path")?;
    Ok(OrderedRow {
        payload_bytes: usize_to_u64(content.len(), "memory content length")?,
        entity_key: document_key.clone(),
        order_rank,
        order_text: order_text.clone(),
        item: MemoryDocument {
            document_id: encode_entity_id(MEMORY_DOCUMENT_ID_PREFIX, &document_key),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            native_project_key: query_get(row, 2, "decode native memory project key")?,
            native_document_path: order_text,
            title: query_get(row, 4, "decode memory title")?,
            content,
            size_bytes: decode_nonnegative_u64(
                query_get(row, 6, "decode memory size")?,
                "memory size",
            )?,
            is_index: query_get::<i64>(row, 7, "decode memory index flag")? != 0,
            resolution_status: query_get(row, 8, "decode memory resolution")?,
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
            assertion_count: decode_nonnegative_u64(
                query_get(row, 10, "decode memory assertion count")?,
                "memory assertion count",
            )?,
            competing_document_count: decode_nonnegative_u64(
                query_get(row, 11, "decode memory competing count")?,
                "memory competing count",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 12, "decode memory commit")?,
                "memory commit",
            )?,
            observed_at_unix_ms: query_get(row, 13, "decode memory observed time")?,
            source_object_id: decode_nonnegative_u64(
                query_get(row, 14, "decode memory source object")?,
                "memory source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 15, "decode memory source generation")?,
                "memory source generation",
            )?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 16, "decode memory source instance")?,
                "memory source instance",
            )?,
            adapter_id: query_get(row, 17, "decode memory adapter")?,
        },
    })
}

fn decode_task_collection_row(
    row: &Row<'_>,
) -> Result<OrderedRow<TaskCollectionSummary>, EngineError> {
    let collection_key: Vec<u8> = query_get(row, 0, "decode task collection key")?;
    let project_key: Option<Vec<u8>> = query_get(row, 1, "decode task project key")?;
    let session_key: Option<Vec<u8>> = query_get(row, 2, "decode task session key")?;
    let run_key: Option<Vec<u8>> = query_get(row, 3, "decode task run key")?;
    let team_key: Option<Vec<u8>> = query_get(row, 4, "decode task team key")?;
    let native_collection_id: String = query_get(row, 5, "decode native task collection id")?;
    let fact_id: Vec<u8> = query_get(row, 10, "decode task collection fact id")?;
    Ok(OrderedRow {
        entity_key: collection_key.clone(),
        order_rank: 0,
        order_text: native_collection_id.clone(),
        payload_bytes: 0,
        item: TaskCollectionSummary {
            collection_id: encode_entity_id(TASK_COLLECTION_ID_PREFIX, &collection_key),
            project_id: project_key
                .as_deref()
                .map(|key| encode_entity_id(PROJECT_ID_PREFIX, key)),
            session_id: session_key
                .as_deref()
                .map(|key| encode_entity_id(SESSION_ID_PREFIX, key)),
            run_id: run_key
                .as_deref()
                .map(|key| encode_entity_id(RUN_ID_PREFIX, key)),
            team_id: team_key
                .as_deref()
                .map(|key| encode_entity_id(TEAM_ID_PREFIX, key)),
            native_collection_id,
            native_owner_id: query_get(row, 6, "decode task collection owner")?,
            collection_kind: query_get(row, 7, "decode task collection kind")?,
            native_collection_kind: query_get(row, 8, "decode native task collection kind")?,
            resolution_status: query_get(row, 9, "decode task collection resolution")?,
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
            assertion_count: decode_nonnegative_u64(
                query_get(row, 11, "decode task collection assertions")?,
                "task collection assertions",
            )?,
            competing_metadata_count: decode_nonnegative_u64(
                query_get(row, 12, "decode task collection conflicts")?,
                "task collection conflicts",
            )?,
            complete_snapshot_count: decode_nonnegative_u64(
                query_get(row, 13, "decode complete task snapshots")?,
                "complete task snapshots",
            )?,
            item_document_count: decode_nonnegative_u64(
                query_get(row, 14, "decode task item documents")?,
                "task item documents",
            )?,
            item_count: decode_nonnegative_u64(
                query_get(row, 15, "decode task item count")?,
                "task item count",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 16, "decode task collection commit")?,
                "task collection commit",
            )?,
            observed_at_unix_ms: query_get(row, 17, "decode task collection observed time")?,
            source_object_id: decode_nonnegative_u64(
                query_get(row, 18, "decode task collection source object")?,
                "task collection source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 19, "decode task collection source generation")?,
                "task collection source generation",
            )?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 20, "decode task collection source instance")?,
                "task collection source instance",
            )?,
            adapter_id: query_get(row, 21, "decode task collection adapter")?,
        },
    })
}

fn decode_task_row(row: &Row<'_>) -> Result<OrderedRow<TaskDetail>, EngineError> {
    let task_key: Vec<u8> = query_get(row, 0, "decode task key")?;
    let collection_key: Vec<u8> = query_get(row, 1, "decode task collection key")?;
    let item_ordinal =
        decode_nonnegative_u64(query_get(row, 2, "decode task ordinal")?, "task ordinal")?;
    let subject: String = query_get(row, 4, "decode task subject")?;
    let description: Option<String> = query_get(row, 5, "decode task description")?;
    let active_form: Option<String> = query_get(row, 6, "decode task active form")?;
    let native_owner: Option<String> = query_get(row, 7, "decode task owner")?;
    let blocks_json: Vec<u8> = query_get(row, 10, "decode task blocks JSON")?;
    let blocked_by_json: Vec<u8> = query_get(row, 11, "decode task blocked-by JSON")?;
    let blocks = decode_string_array(&blocks_json, "task blocks")?;
    let blocked_by = decode_string_array(&blocked_by_json, "task blocked-by")?;
    let fact_id: Vec<u8> = query_get(row, 13, "decode task fact id")?;
    let payload_bytes = checked_payload_sum(
        [
            subject.len(),
            description.as_ref().map_or(0, String::len),
            active_form.as_ref().map_or(0, String::len),
            native_owner.as_ref().map_or(0, String::len),
            blocks_json.len(),
            blocked_by_json.len(),
        ],
        "task payload",
    )?;
    Ok(OrderedRow {
        entity_key: task_key.clone(),
        order_rank: item_ordinal,
        order_text: String::new(),
        payload_bytes,
        item: TaskDetail {
            task_id: encode_entity_id(TASK_ID_PREFIX, &task_key),
            collection_id: encode_entity_id(TASK_COLLECTION_ID_PREFIX, &collection_key),
            item_ordinal,
            native_task_id: query_get(row, 3, "decode native task id")?,
            subject,
            description,
            active_form,
            native_owner,
            task_status: query_get(row, 8, "decode task status")?,
            native_status: query_get(row, 9, "decode native task status")?,
            blocks,
            blocked_by,
            resolution_status: query_get(row, 12, "decode task resolution")?,
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
            assertion_count: decode_nonnegative_u64(
                query_get(row, 14, "decode task assertions")?,
                "task assertions",
            )?,
            competing_item_count: decode_nonnegative_u64(
                query_get(row, 15, "decode task competing items")?,
                "task competing items",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 16, "decode task commit")?,
                "task commit",
            )?,
            observed_at_unix_ms: query_get(row, 17, "decode task observed time")?,
            source_object_id: decode_nonnegative_u64(
                query_get(row, 18, "decode task source object")?,
                "task source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 19, "decode task source generation")?,
                "task source generation",
            )?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 20, "decode task source instance")?,
                "task source instance",
            )?,
            adapter_id: query_get(row, 21, "decode task adapter")?,
        },
    })
}

fn decode_plan_row(row: &Row<'_>) -> Result<OrderedRow<PlanDetail>, EngineError> {
    let plan_key: Vec<u8> = query_get(row, 0, "decode plan key")?;
    let native_plan_id: String = query_get(row, 1, "decode native plan id")?;
    let content: String = query_get(row, 3, "decode plan content")?;
    let fact_id: Vec<u8> = query_get(row, 8, "decode plan fact id")?;
    Ok(OrderedRow {
        payload_bytes: usize_to_u64(content.len(), "plan content length")?,
        entity_key: plan_key.clone(),
        order_rank: 0,
        order_text: native_plan_id.clone(),
        item: PlanDetail {
            plan_id: encode_entity_id(PLAN_ID_PREFIX, &plan_key),
            native_plan_id,
            title: query_get(row, 2, "decode plan title")?,
            content,
            size_bytes: decode_nonnegative_u64(
                query_get(row, 4, "decode plan size")?,
                "plan size",
            )?,
            source_time: query_get(row, 5, "decode plan source time")?,
            source_time_quality: query_get(row, 6, "decode plan time quality")?,
            resolution_status: query_get(row, 7, "decode plan resolution")?,
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
            assertion_count: decode_nonnegative_u64(
                query_get(row, 9, "decode plan assertions")?,
                "plan assertions",
            )?,
            competing_plan_count: decode_nonnegative_u64(
                query_get(row, 10, "decode competing plans")?,
                "competing plans",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 11, "decode plan commit")?,
                "plan commit",
            )?,
            observed_at_unix_ms: query_get(row, 12, "decode plan observed time")?,
            source_object_id: decode_nonnegative_u64(
                query_get(row, 13, "decode plan source object")?,
                "plan source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 14, "decode plan source generation")?,
                "plan source generation",
            )?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 15, "decode plan source instance")?,
                "plan source instance",
            )?,
            adapter_id: query_get(row, 16, "decode plan adapter")?,
        },
    })
}

fn decode_tool_result_row(row: &Row<'_>) -> Result<OrderedRow<ToolResultDetail>, EngineError> {
    let result_key: Vec<u8> = query_get(row, 0, "decode tool result key")?;
    let project_key: Vec<u8> = query_get(row, 1, "decode tool result project key")?;
    let session_key: Vec<u8> = query_get(row, 2, "decode tool result session key")?;
    let native_tool_use_id: String = query_get(row, 5, "decode native tool use id")?;
    let content: String = query_get(row, 7, "decode tool result content")?;
    let tool_call_key: Option<Vec<u8>> = query_get(row, 11, "decode tool call message key")?;
    let tool_result_key: Option<Vec<u8>> =
        query_get(row, 12, "decode inline tool result message key")?;
    let fact_id: Vec<u8> = query_get(row, 13, "decode tool result fact id")?;
    Ok(OrderedRow {
        payload_bytes: usize_to_u64(content.len(), "tool result content length")?,
        entity_key: result_key.clone(),
        order_rank: 0,
        order_text: native_tool_use_id.clone(),
        item: ToolResultDetail {
            result_id: encode_entity_id(TOOL_RESULT_ID_PREFIX, &result_key),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            native_project_key: query_get(row, 3, "decode native tool result project")?,
            native_session_id: query_get(row, 4, "decode native tool result session")?,
            native_tool_use_id,
            native_document_path: query_get(row, 6, "decode tool result path")?,
            content,
            size_bytes: decode_nonnegative_u64(
                query_get(row, 8, "decode tool result size")?,
                "tool result size",
            )?,
            resolution_status: query_get(row, 9, "decode tool result resolution")?,
            correlation_status: query_get(row, 10, "decode tool result correlation")?,
            tool_call_message_id: tool_call_key
                .as_deref()
                .map(|key| encode_entity_id(MESSAGE_ID_PREFIX, key)),
            tool_result_message_id: tool_result_key
                .as_deref()
                .map(|key| encode_entity_id(MESSAGE_ID_PREFIX, key)),
            decisive_fact_id: encode_entity_id(FACT_ID_PREFIX, &fact_id),
            assertion_count: decode_nonnegative_u64(
                query_get(row, 14, "decode tool result assertions")?,
                "tool result assertions",
            )?,
            competing_result_count: decode_nonnegative_u64(
                query_get(row, 15, "decode competing tool results")?,
                "competing tool results",
            )?,
            tool_call_match_count: decode_nonnegative_u64(
                query_get(row, 16, "decode tool call matches")?,
                "tool call matches",
            )?,
            tool_result_match_count: decode_nonnegative_u64(
                query_get(row, 17, "decode tool result matches")?,
                "tool result matches",
            )?,
            join_conflict: query_get::<i64>(row, 18, "decode tool result join conflict")? != 0,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 19, "decode tool result commit")?,
                "tool result commit",
            )?,
            observed_at_unix_ms: query_get(row, 20, "decode tool result observed time")?,
            source_object_id: decode_nonnegative_u64(
                query_get(row, 21, "decode tool result source object")?,
                "tool result source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 22, "decode tool result source generation")?,
                "tool result source generation",
            )?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 23, "decode tool result source instance")?,
                "tool result source instance",
            )?,
            adapter_id: query_get(row, 24, "decode tool result adapter")?,
        },
    })
}

fn decode_artifact_row(row: &Row<'_>) -> Result<OrderedRow<ArtifactDetail>, EngineError> {
    let artifact_key: Vec<u8> = query_get(row, 0, "decode artifact key")?;
    let session_key: Vec<u8> = query_get(row, 1, "decode artifact session key")?;
    let project_key: Option<Vec<u8>> = query_get(row, 2, "decode artifact project key")?;
    let content: Option<Vec<u8>> = query_get(row, 11, "decode artifact content")?;
    let content_base64 = content.as_deref().map(|bytes| STANDARD.encode(bytes));
    let content_digest: Option<Vec<u8>> = query_get(row, 13, "decode artifact content digest")?;
    let metadata_fact: Option<Vec<u8>> = query_get(row, 16, "decode artifact metadata fact")?;
    let content_fact: Option<Vec<u8>> = query_get(row, 17, "decode artifact content fact")?;
    let order_rank = decode_nonnegative_u64(
        query_get(row, 34, "decode artifact order rank")?,
        "artifact order rank",
    )?;
    let order_text: String = query_get(row, 35, "decode artifact order time")?;
    Ok(OrderedRow {
        payload_bytes: content_base64.as_ref().map_or(Ok(0), |value| {
            usize_to_u64(value.len(), "artifact base64 length")
        })?,
        entity_key: artifact_key.clone(),
        order_rank,
        order_text,
        item: ArtifactDetail {
            artifact_id: encode_entity_id(ARTIFACT_ID_PREFIX, &artifact_key),
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            project_id: project_key
                .as_deref()
                .map(|key| encode_entity_id(PROJECT_ID_PREFIX, key)),
            native_artifact_id: query_get(row, 3, "decode native artifact id")?,
            native_file_hash: query_get(row, 4, "decode native artifact hash")?,
            version: decode_nonnegative_u64(
                query_get(row, 5, "decode artifact version")?,
                "artifact version",
            )?,
            tracking_path: query_get(row, 6, "decode artifact tracking path")?,
            real_parent_dir: query_get(row, 7, "decode artifact parent directory")?,
            backup_time: query_get(row, 8, "decode artifact backup time")?,
            backup_time_quality: query_get(row, 9, "decode artifact time quality")?,
            capture_status: query_get(row, 10, "decode artifact capture status")?,
            content_base64,
            size_bytes: decode_optional_u64(
                query_get(row, 12, "decode artifact size")?,
                "artifact size",
            )?,
            content_digest_base64url: content_digest
                .as_deref()
                .map(|digest| URL_SAFE_NO_PAD.encode(digest)),
            content_status: query_get(row, 14, "decode artifact content status")?,
            resolution_status: query_get(row, 15, "decode artifact resolution")?,
            metadata_fact_id: metadata_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            content_fact_id: content_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            metadata_assertion_count: decode_nonnegative_u64(
                query_get(row, 18, "decode artifact metadata assertions")?,
                "artifact metadata assertions",
            )?,
            competing_metadata_count: decode_nonnegative_u64(
                query_get(row, 19, "decode artifact metadata conflicts")?,
                "artifact metadata conflicts",
            )?,
            content_assertion_count: decode_nonnegative_u64(
                query_get(row, 20, "decode artifact content assertions")?,
                "artifact content assertions",
            )?,
            competing_content_count: decode_nonnegative_u64(
                query_get(row, 21, "decode artifact content conflicts")?,
                "artifact content conflicts",
            )?,
            join_conflict: query_get::<i64>(row, 22, "decode artifact join conflict")? != 0,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 23, "decode artifact commit")?,
                "artifact commit",
            )?,
            metadata_adapter_id: query_get(row, 28, "decode artifact metadata adapter")?,
            metadata_source_instance_id: decode_optional_u64(
                query_get(row, 27, "decode artifact metadata source instance")?,
                "artifact metadata source instance",
            )?,
            metadata_observed_at_unix_ms: query_get(
                row,
                24,
                "decode artifact metadata observed time",
            )?,
            metadata_source_object_id: decode_optional_u64(
                query_get(row, 25, "decode artifact metadata source object")?,
                "artifact metadata source object",
            )?,
            metadata_source_generation: decode_optional_u64(
                query_get(row, 26, "decode artifact metadata source generation")?,
                "artifact metadata source generation",
            )?,
            content_adapter_id: query_get(row, 33, "decode artifact content adapter")?,
            content_source_instance_id: decode_optional_u64(
                query_get(row, 32, "decode artifact content source instance")?,
                "artifact content source instance",
            )?,
            content_observed_at_unix_ms: query_get(
                row,
                29,
                "decode artifact content observed time",
            )?,
            content_source_object_id: decode_optional_u64(
                query_get(row, 30, "decode artifact content source object")?,
                "artifact content source object",
            )?,
            content_source_generation: decode_optional_u64(
                query_get(row, 31, "decode artifact content source generation")?,
                "artifact content source generation",
            )?,
        },
    })
}

fn collect_bounded_rows<T>(
    rows: &mut rusqlite::Rows<'_>,
    limit: u32,
    decode: fn(&Row<'_>) -> Result<OrderedRow<T>, EngineError>,
    label: &'static str,
) -> Result<(Vec<OrderedRow<T>>, u64, bool), EngineError> {
    let mut items = Vec::new();
    let mut payload_bytes = 0_u64;
    let mut has_more = false;
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance capability page", error))?
    {
        if items.len() >= limit as usize {
            has_more = true;
            break;
        }
        let decoded = decode(row)?;
        let next_bytes = payload_bytes
            .checked_add(decoded.payload_bytes)
            .ok_or_else(|| EngineError::Sqlite {
                operation: "bound capability payload",
                detail: format!("{label} payload byte total overflowed u64"),
            })?;
        if next_bytes > MAX_CAPABILITY_PAGE_PAYLOAD_BYTES {
            if items.is_empty() {
                return Err(EngineError::Sqlite {
                    operation: "bound capability payload",
                    detail: format!(
                        "one {label} row requires {} payload bytes; maximum is {MAX_CAPABILITY_PAGE_PAYLOAD_BYTES}",
                        decoded.payload_bytes
                    ),
                });
            }
            has_more = true;
            break;
        }
        payload_bytes = next_bytes;
        items.push(decoded);
    }
    Ok((items, payload_bytes, has_more))
}

fn task_collection_scope(request: &TaskCollectionPageRequest) -> Vec<String> {
    if let Some(value) = &request.session_id {
        vec!["session".to_string(), value.clone()]
    } else if let Some(value) = &request.run_id {
        vec!["run".to_string(), value.clone()]
    } else if let Some(value) = &request.team_id {
        vec!["team".to_string(), value.clone()]
    } else {
        vec!["all".to_string()]
    }
}

fn validate_request_cursor(
    value: Option<&str>,
    kind: CapabilityCursorKind,
    scope: &[String],
) -> Result<(), EngineError> {
    decode_optional_cursor(value, kind, scope).map(|_| ())
}

fn decode_optional_cursor(
    value: Option<&str>,
    kind: CapabilityCursorKind,
    scope: &[String],
) -> Result<Option<CapabilityCursor>, EngineError> {
    value
        .map(|value| decode_cursor(value, kind, scope))
        .transpose()
}

fn decode_cursor(
    value: &str,
    kind: CapabilityCursorKind,
    scope: &[String],
) -> Result<CapabilityCursor, EngineError> {
    if value.is_empty() || value.len() > MAX_CAPABILITY_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "capability cursor is empty or exceeds the supported bound".to_string(),
        ));
    }
    let json = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("capability cursor is not valid base64url".to_string())
    })?;
    let cursor: CapabilityCursor = serde_json::from_slice(&json).map_err(|_| {
        EngineError::InvalidQuery("capability cursor payload is malformed".to_string())
    })?;
    if cursor.version != CAPABILITY_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported capability cursor version {}",
            cursor.version
        )));
    }
    if cursor.kind != kind || cursor.scope != scope {
        return Err(EngineError::InvalidQuery(
            "capability cursor does not belong to this query scope".to_string(),
        ));
    }
    if cursor.order_text.len() > MAX_CAPABILITY_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "capability cursor order key exceeds the supported bound".to_string(),
        ));
    }
    to_query_i64(cursor.order_rank, "capability cursor order rank")?;
    let entity_key = URL_SAFE_NO_PAD.decode(&cursor.entity_key).map_err(|_| {
        EngineError::InvalidQuery("capability cursor entity key is malformed".to_string())
    })?;
    if entity_key.is_empty() || entity_key.len() > MAX_CAPABILITY_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "capability cursor entity key is outside the supported bound".to_string(),
        ));
    }
    Ok(cursor)
}

fn page_cursor<T>(
    has_more: bool,
    last: Option<&OrderedRow<T>>,
    kind: CapabilityCursorKind,
    watermark: u64,
    scope: Vec<String>,
) -> Result<Option<String>, EngineError> {
    if !has_more {
        return Ok(None);
    }
    last.map(|row| {
        encode_cursor(&CapabilityCursor {
            version: CAPABILITY_QUERY_CONTRACT_VERSION,
            kind,
            at_commit_seq: watermark,
            scope,
            order_rank: row.order_rank,
            order_text: row.order_text.clone(),
            entity_key: URL_SAFE_NO_PAD.encode(&row.entity_key),
        })
    })
    .transpose()
}

fn encode_cursor(cursor: &CapabilityCursor) -> Result<String, EngineError> {
    let json = serde_json::to_vec(cursor).map_err(|error| {
        EngineError::InvalidQuery(format!("could not encode capability cursor: {error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn cursor_entity_key(cursor: Option<&CapabilityCursor>) -> Result<Vec<u8>, EngineError> {
    cursor
        .map(|cursor| {
            URL_SAFE_NO_PAD.decode(&cursor.entity_key).map_err(|_| {
                EngineError::InvalidQuery("capability cursor entity key is malformed".to_string())
            })
        })
        .transpose()
        .map(|value| value.unwrap_or_default())
}

fn validate_cursor_watermark(
    cursor: Option<&CapabilityCursor>,
    watermark: u64,
    label: &'static str,
) -> Result<(), EngineError> {
    if let Some(cursor) = cursor {
        if cursor.at_commit_seq != watermark {
            return Err(EngineError::InvalidQuery(format!(
                "{label} cursor expired at commit {}; current commit is {watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    Ok(())
}

fn begin_snapshot<'a>(
    connection: &'a Connection,
    label: &'static str,
) -> Result<Transaction<'a>, EngineError> {
    connection
        .unchecked_transaction()
        .map_err(|error| EngineError::Sqlite {
            operation: "begin capability snapshot",
            detail: format!("{label}: {error}"),
        })
}

fn finish_snapshot(transaction: Transaction<'_>, _label: &'static str) -> Result<(), EngineError> {
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish capability snapshot", error))
}

fn validate_page_limit(limit: u32, label: &'static str) -> Result<(), EngineError> {
    if !(1..=MAX_CAPABILITY_PAGE_LIMIT).contains(&limit) {
        return Err(EngineError::InvalidQuery(format!(
            "{label} page limit must be between 1 and {MAX_CAPABILITY_PAGE_LIMIT}, got {limit}"
        )));
    }
    Ok(())
}

fn require_session_membership(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    session_key: &[u8],
    label: &'static str,
) -> Result<(), EngineError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM canonical_sessions WHERE session_key = ?1 AND project_key = ?2",
            rusqlite::params![session_key, project_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("validate capability session scope", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(format!(
            "{label} session id does not identify a current session in the requested project"
        )));
    }
    Ok(())
}

fn decode_string_array(bytes: &[u8], label: &'static str) -> Result<Vec<String>, EngineError> {
    serde_json::from_slice(bytes).map_err(|error| EngineError::Sqlite {
        operation: "decode capability JSON",
        detail: format!("{label}: {error}"),
    })
}

fn checked_payload_sum(
    values: impl IntoIterator<Item = usize>,
    label: &'static str,
) -> Result<u64, EngineError> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(usize_to_u64(value, label)?)
            .ok_or_else(|| EngineError::Sqlite {
                operation: "measure capability payload",
                detail: format!("{label} overflowed u64"),
            })
    })
}

fn usize_to_u64(value: usize, label: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "measure capability payload",
        detail: format!("{label} exceeded u64"),
    })
}

fn query_get<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    index: usize,
    operation: &'static str,
) -> Result<T, EngineError> {
    row.get(index)
        .map_err(|error| query_sqlite_error(operation, error))
}

fn decode_nonnegative_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode capability integer",
        detail: format!("{field} was negative: {value}"),
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

    fn seeded_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_instances VALUES (1, 'fixture', ?1, 'Fixture', '1.0.0', 1, '[]', '[]', 10, 20)",
                [b"fixture-root".as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_streams VALUES (1, 1, 'fixture', 'replace_document', 'fixture', 'available', 'none', 20, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_objects (
                    source_object_id, source_stream_id, object_key, display_path,
                    generation, committed_cursor, size_bytes, mtime_ns,
                    decoder_contract_version, last_commit_seq, state
                ) VALUES (1, 1, ?1, '/fixture', 1, ?2, 100, 20, 1, 1, 'active')",
                params![b"object".as_slice(), b"cursor".as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (1, 1, 'fixture', 1, 2, 20)",
                [],
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
                          '/fixture/project', NULL, NULL, NULL, NULL, NULL,
                          NULL, ?3, 1, 1, ?4, 1)
                "#,
                params![
                    b"session".as_slice(),
                    b"project".as_slice(),
                    b"session-fact".as_slice(),
                    b"session-cursor".as_slice(),
                ],
            )
            .unwrap();

        for (ordinal, key, path, is_index, content) in [
            (2, b"memory-topic".as_slice(), "memory/topic.md", 0, "topic"),
            (
                3,
                b"memory-index".as_slice(),
                "memory/MEMORY.md",
                1,
                "index",
            ),
        ] {
            let fact = format!("memory-fact-{ordinal}").into_bytes();
            insert_fact(&connection, &fact, "project_memory_document", key, ordinal);
            connection
                .execute(
                    r#"
                    INSERT INTO project_memory_document_assertions VALUES (
                        ?1, ?2, ?3, 'native-project', ?4, ?5, ?6, ?7, ?8,
                        ?9, 1, 1, ?10, 1
                    )
                    "#,
                    params![
                        fact,
                        key,
                        b"project".as_slice(),
                        path,
                        path,
                        content,
                        content.len() as i64,
                        is_index,
                        [ordinal as u8; 32].as_slice(),
                        b"cursor".as_slice(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    r#"
                    INSERT INTO canonical_project_memory_documents VALUES (
                        ?1, ?2, 'native-project', ?3, ?4, ?5, ?6, ?7,
                        'resolved', ?8, 1, 0, 1
                    )
                    "#,
                    params![
                        key,
                        b"project".as_slice(),
                        path,
                        path,
                        content,
                        content.len() as i64,
                        is_index,
                        fact,
                    ],
                )
                .unwrap();
        }

        for (ordinal, collection, native_id, session) in [
            (
                4,
                b"collection-a".as_slice(),
                "a-list",
                Some(b"session".as_slice()),
            ),
            (5, b"collection-b".as_slice(), "b-list", None),
        ] {
            let fact = format!("task-fact-{ordinal}").into_bytes();
            insert_fact(&connection, &fact, "task_snapshot", collection, ordinal);
            connection
                .execute(
                    r#"
                    INSERT INTO task_snapshot_assertions VALUES (
                        ?1, ?2, ?3, NULL, NULL, ?4, NULL, 'todo_list',
                        'todo_list', 'complete', ?5, 1, 1, ?6, 1
                    )
                    "#,
                    params![
                        fact,
                        collection,
                        session,
                        native_id,
                        [ordinal as u8; 32].as_slice(),
                        b"cursor".as_slice(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    r#"
                    INSERT INTO canonical_task_collections VALUES (
                        ?1, ?2, NULL, NULL, ?3, NULL, 'todo_list',
                        'todo_list', 'resolved', ?4, 1, 0, 1, 0, 1, 1
                    )
                    "#,
                    params![collection, session, native_id, fact],
                )
                .unwrap();
            let task = format!("task-{ordinal}").into_bytes();
            connection
                .execute(
                    r#"
                    INSERT INTO task_item_assertions VALUES (
                        ?1, ?2, ?3, 0, NULL, ?4, NULL, NULL, NULL,
                        'pending', 'pending', ?5, ?6, ?7
                    )
                    "#,
                    params![
                        fact,
                        task,
                        collection,
                        format!("subject-{ordinal}"),
                        br#"["2"]"#.as_slice(),
                        br#"["0"]"#.as_slice(),
                        [ordinal as u8; 32].as_slice(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    r#"
                    INSERT INTO canonical_tasks VALUES (
                        ?1, ?2, 0, NULL, ?3, NULL, NULL, NULL,
                        'pending', 'pending', ?4, ?5, 'resolved', ?6,
                        1, 0, 1
                    )
                    "#,
                    params![
                        task,
                        collection,
                        format!("subject-{ordinal}"),
                        br#"["2"]"#.as_slice(),
                        br#"["0"]"#.as_slice(),
                        fact,
                    ],
                )
                .unwrap();
        }

        for (ordinal, key, native_id) in [
            (6, b"plan-b".as_slice(), "b-plan"),
            (7, b"plan-a".as_slice(), "a-plan"),
        ] {
            let fact = format!("plan-fact-{ordinal}").into_bytes();
            insert_fact(&connection, &fact, "plan_snapshot", key, ordinal);
            connection
                .execute(
                    r#"
                    INSERT INTO plan_assertions VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, 1, 1,
                        ?8, 1
                    )
                    "#,
                    params![
                        fact,
                        key,
                        native_id,
                        native_id,
                        format!("content-{native_id}"),
                        native_id.len() as i64 + 8,
                        [ordinal as u8; 32].as_slice(),
                        b"cursor".as_slice(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    r#"
                    INSERT INTO canonical_plans VALUES (
                        ?1, ?2, ?3, ?4, ?5, NULL, NULL, 'resolved', ?6,
                        1, 0, 1
                    )
                    "#,
                    params![
                        key,
                        native_id,
                        native_id,
                        format!("content-{native_id}"),
                        native_id.len() as i64 + 8,
                        fact,
                    ],
                )
                .unwrap();
        }

        for (ordinal, key, native_id, content) in [
            (8, b"result-b".as_slice(), "tool-b", "result-b"),
            (9, b"result-a".as_slice(), "tool-a", "result-a"),
        ] {
            let fact = format!("result-fact-{ordinal}").into_bytes();
            insert_fact(&connection, &fact, "persisted_tool_result", key, ordinal);
            connection
                .execute(
                    r#"
                    INSERT INTO persisted_tool_result_assertions VALUES (
                        ?1, ?2, ?3, ?4, 'native-project', 'native-session',
                        ?5, ?6, ?7, ?8, ?9, 1, 1, ?10, 1
                    )
                    "#,
                    params![
                        fact,
                        key,
                        b"session".as_slice(),
                        b"project".as_slice(),
                        native_id,
                        format!("tool-results/{native_id}.txt"),
                        content,
                        content.len() as i64,
                        [ordinal as u8; 32].as_slice(),
                        b"cursor".as_slice(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    r#"
                    INSERT INTO canonical_persisted_tool_results VALUES (
                        ?1, ?2, ?3, 'native-project', 'native-session', ?4,
                        ?5, ?6, ?7, 'resolved', 'unlinked', NULL, NULL, ?8,
                        1, 0, 0, 0, 0, 1
                    )
                    "#,
                    params![
                        key,
                        b"session".as_slice(),
                        b"project".as_slice(),
                        native_id,
                        format!("tool-results/{native_id}.txt"),
                        content,
                        content.len() as i64,
                        fact,
                    ],
                )
                .unwrap();
        }

        let artifact_content = [0_u8, 1, 2, 255];
        let artifact_fact = b"artifact-fact".as_slice();
        insert_fact(
            &connection,
            artifact_fact,
            "artifact_content",
            b"artifact",
            10,
        );
        connection
            .execute(
                r#"
                INSERT INTO artifact_content_assertions VALUES (
                    ?1, ?2, ?3, 'hash@v1', 'hash', 1, ?4, 4, ?5, ?6,
                    1, 1, ?7, 1
                )
                "#,
                params![
                    artifact_fact,
                    b"artifact".as_slice(),
                    b"session".as_slice(),
                    artifact_content.as_slice(),
                    [10_u8; 32].as_slice(),
                    [11_u8; 32].as_slice(),
                    b"cursor".as_slice(),
                ],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO canonical_artifacts VALUES (
                    ?1, ?2, 'hash@v1', 'hash', 1, NULL, NULL, NULL, NULL,
                    'unknown', ?3, 4, ?4, 'orphan_content', 'incomplete',
                    NULL, ?5, 0, 0, 1, 0, 0, 1
                )
                "#,
                params![
                    b"artifact".as_slice(),
                    b"session".as_slice(),
                    artifact_content.as_slice(),
                    [10_u8; 32].as_slice(),
                    artifact_fact,
                ],
            )
            .unwrap();
        connection
    }

    #[test]
    fn capability_pages_preserve_scope_order_payload_and_provenance() {
        let connection = seeded_connection();
        let project_id = encode_entity_id(PROJECT_ID_PREFIX, b"project");
        let session_id = encode_entity_id(SESSION_ID_PREFIX, b"session");

        let first_memory = read_memory_document_page(
            &connection,
            &MemoryDocumentPageRequest {
                project_id: project_id.clone(),
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        assert!(first_memory.items[0].is_index);
        assert_eq!(first_memory.items[0].content, "index");
        assert!(first_memory.items[0]
            .document_id
            .starts_with(MEMORY_DOCUMENT_ID_PREFIX));
        assert!(first_memory.items[0]
            .decisive_fact_id
            .starts_with(FACT_ID_PREFIX));
        let second_memory = read_memory_document_page(
            &connection,
            &MemoryDocumentPageRequest {
                project_id: project_id.clone(),
                cursor: first_memory.next_cursor,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(second_memory.items[0].content, "topic");
        assert!(second_memory.next_cursor.is_none());

        let scoped_collections = read_task_collection_page(
            &connection,
            &TaskCollectionPageRequest {
                session_id: Some(session_id.clone()),
                run_id: None,
                team_id: None,
                cursor: None,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(scoped_collections.items.len(), 1);
        assert_eq!(scoped_collections.items[0].native_collection_id, "a-list");
        assert_eq!(
            scoped_collections.items[0].project_id.as_deref(),
            Some(project_id.as_str())
        );
        let collection_id = scoped_collections.items[0].collection_id.clone();
        let tasks = read_task_page(
            &connection,
            &TaskPageRequest {
                collection_id,
                cursor: None,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(tasks.items[0].blocks, ["2"]);
        assert_eq!(tasks.items[0].blocked_by, ["0"]);
        assert!(tasks.payload_bytes > 0);

        let first_plan = read_plan_page(
            &connection,
            &PlanPageRequest {
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(first_plan.items[0].native_plan_id, "a-plan");
        let second_plan = read_plan_page(
            &connection,
            &PlanPageRequest {
                cursor: first_plan.next_cursor,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(second_plan.items[0].native_plan_id, "b-plan");

        let first_result = read_tool_result_page(
            &connection,
            &ToolResultPageRequest {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(first_result.items[0].native_tool_use_id, "tool-a");
        assert_eq!(first_result.items[0].content, "result-a");
        assert_eq!(first_result.items[0].correlation_status, "unlinked");
        assert!(first_result.next_cursor.is_some());

        let artifacts = read_artifact_page(
            &connection,
            &ArtifactPageRequest {
                session_id,
                cursor: None,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(artifacts.items.len(), 1);
        assert_eq!(
            artifacts.items[0].content_base64.as_deref(),
            Some("AAEC/w==")
        );
        assert_eq!(artifacts.items[0].content_status, "orphan_content");
        assert!(artifacts.items[0].metadata_adapter_id.is_none());
        assert_eq!(
            artifacts.items[0].content_adapter_id.as_deref(),
            Some("fixture")
        );
        assert_eq!(artifacts.items[0].content_source_instance_id, Some(1));
        assert_eq!(artifacts.payload_bytes, 8);
    }

    #[test]
    fn capability_cursors_reject_cross_scope_and_expire_after_commit() {
        let connection = seeded_connection();
        let project_id = encode_entity_id(PROJECT_ID_PREFIX, b"project");
        let first = read_memory_document_page(
            &connection,
            &MemoryDocumentPageRequest {
                project_id: project_id.clone(),
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        let cursor = first.next_cursor.unwrap();
        assert!(matches!(
            validate_memory_document_page(&MemoryDocumentPageRequest {
                project_id: encode_entity_id(PROJECT_ID_PREFIX, b"other"),
                cursor: Some(cursor.clone()),
                limit: 1,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (2, 1, 'later', 3, 4, 0)",
                [],
            )
            .unwrap();
        assert!(matches!(
            read_memory_document_page(
                &connection,
                &MemoryDocumentPageRequest {
                    project_id,
                    cursor: Some(cursor),
                    limit: 1,
                }
            ),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn capability_requests_reject_invalid_identity_scope_and_limits() {
        let connection = seeded_connection();
        let session_id = encode_entity_id(SESSION_ID_PREFIX, b"session");
        assert!(matches!(
            validate_task_collection_page(&TaskCollectionPageRequest {
                session_id: Some(session_id.clone()),
                run_id: Some(encode_entity_id(RUN_ID_PREFIX, b"run")),
                team_id: None,
                cursor: None,
                limit: 1,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            validate_artifact_page(&ArtifactPageRequest {
                session_id: "not-a-session".to_string(),
                cursor: None,
                limit: 1,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            validate_plan_page(&PlanPageRequest {
                cursor: None,
                limit: 0,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            read_tool_result_page(
                &connection,
                &ToolResultPageRequest {
                    project_id: encode_entity_id(PROJECT_ID_PREFIX, b"other"),
                    session_id,
                    cursor: None,
                    limit: 1,
                }
            ),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn single_oversized_capability_row_is_rejected() {
        let connection = seeded_connection();
        let content = "x".repeat(usize::try_from(MAX_CAPABILITY_PAGE_PAYLOAD_BYTES).unwrap() + 1);
        connection
            .execute(
                "UPDATE canonical_plans SET content = ?1 WHERE native_plan_id = 'a-plan'",
                [content],
            )
            .unwrap();
        let error = read_plan_page(
            &connection,
            &PlanPageRequest {
                cursor: None,
                limit: 1,
            },
        )
        .unwrap_err();
        assert!(matches!(error, EngineError::Sqlite { .. }));
        assert!(error.to_string().contains("maximum is 16777216"));
    }

    #[test]
    fn capability_keyset_indexes_are_installed_and_selected() {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        let cases = [
            (
                "SELECT document_key FROM canonical_project_memory_documents WHERE project_key = ?1 ORDER BY is_index DESC, native_document_path LIMIT 10",
                "idx_canonical_project_memory_documents_project",
            ),
            (
                "SELECT collection_key FROM canonical_task_collections WHERE session_key = ?1 ORDER BY native_collection_id, collection_key LIMIT 10",
                "idx_canonical_task_collections_session_native",
            ),
            (
                "SELECT collection_key FROM canonical_task_collections WHERE run_key = ?1 ORDER BY native_collection_id, collection_key LIMIT 10",
                "idx_canonical_task_collections_run",
            ),
            (
                "SELECT collection_key FROM canonical_task_collections WHERE team_key = ?1 ORDER BY native_collection_id, collection_key LIMIT 10",
                "idx_canonical_task_collections_team",
            ),
            (
                "SELECT plan_key FROM canonical_plans WHERE native_plan_id > ?1 ORDER BY native_plan_id, plan_key LIMIT 10",
                "idx_canonical_plans_native",
            ),
            (
                "SELECT task_key FROM canonical_tasks WHERE collection_key = ?1 ORDER BY item_ordinal, task_key LIMIT 10",
                "idx_canonical_tasks_collection",
            ),
            (
                "SELECT result_key FROM canonical_persisted_tool_results WHERE session_key = ?1 ORDER BY native_tool_use_id, result_key LIMIT 10",
                "idx_canonical_persisted_tool_results_session",
            ),
            (
                "SELECT artifact_key FROM canonical_artifacts WHERE session_key = ?1 ORDER BY backup_time, artifact_key LIMIT 10",
                "idx_canonical_artifacts_session",
            ),
        ];
        for (sql, expected_index) in cases {
            let plan = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map([b"scope".as_slice()], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                plan.iter().any(|step| step.contains(expected_index)),
                "expected {expected_index} in query plan: {plan:?}"
            );
        }
    }
}
