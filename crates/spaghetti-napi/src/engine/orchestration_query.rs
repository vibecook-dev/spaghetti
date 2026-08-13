//! Read-only RFC 011 delegation and workflow query pack.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::query_identity::{
    decode_entity_id, encode_entity_id, FACT_ID_PREFIX, MESSAGE_ID_PREFIX, PROJECT_ID_PREFIX,
    RUN_ID_PREFIX, SESSION_ID_PREFIX, WORKFLOW_ID_PREFIX, WORKFLOW_MEMBER_ID_PREFIX,
};
use super::query_pool::read_committed_watermark;
use super::EngineError;

pub const ORCHESTRATION_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_ORCHESTRATION_PAGE_LIMIT: u32 = 50;
pub const MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ORCHESTRATION_PAGE_LIMIT: u32 = 200;
const MAX_ORCHESTRATION_CURSOR_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationPageRequest {
    pub project_id: String,
    pub session_id: String,
    /// Optional opaque workflow identity. When present, only child runs named
    /// by that workflow's durable member projection are returned.
    pub workflow_id: Option<String>,
    /// Exclude every child run with a current workflow-member relation.
    pub standalone_only: bool,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: String,
    pub workflow_id: Option<String>,
    pub standalone_only: bool,
    pub items: Vec<DelegationSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationSummary {
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub project_id: String,
    pub session_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_run_id: Option<String>,
    pub native_child_id: Option<String>,
    pub native_task_id: Option<String>,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub native_name: Option<String>,
    pub spawn_depth: Option<u32>,
    pub label: Option<String>,
    pub prompt: Option<String>,
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub relation_kind: String,
    pub relation_strength: String,
    pub relation_status: String,
    pub metadata_status: Option<String>,
    pub spawn_status: Option<String>,
    pub branch_tool_name: Option<String>,
    pub requested_agent_type: Option<String>,
    pub branch_anchor_message_id: Option<String>,
    pub child_present: bool,
    pub parent_present: bool,
    pub metadata_run_present: Option<bool>,
    pub observed_run_state: Option<String>,
    pub message_count: u64,
    pub workflow_member_count: u64,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub decisive_relation_fact_id: Option<String>,
    pub decisive_spawn_fact_id: Option<String>,
    pub decisive_metadata_fact_id: Option<String>,
    pub assertion_count: u64,
    pub competing_relation_count: u64,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPageRequest {
    pub project_id: String,
    pub session_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<WorkflowSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSummary {
    pub workflow_id: String,
    pub project_id: String,
    pub session_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_workflow_id: String,
    pub native_task_id: Option<String>,
    pub name: Option<String>,
    pub native_status: Option<String>,
    pub workflow_status: Option<String>,
    pub started_at: Option<String>,
    pub started_at_quality: Option<String>,
    pub finished_at: Option<String>,
    pub finished_at_quality: Option<String>,
    pub duration_ms: Option<u64>,
    pub agent_count: Option<u64>,
    pub total_tokens: Option<u64>,
    pub total_tool_calls: Option<u64>,
    pub snapshot_status: String,
    pub resolution_status: String,
    pub decisive_snapshot_fact_id: Option<String>,
    pub provenance_fact_id: String,
    pub snapshot_assertion_count: u64,
    pub competing_snapshot_count: u64,
    pub observed_member_count: u64,
    pub started_member_count: u64,
    pub result_member_count: u64,
    pub unresolved_member_count: u64,
    pub conflicting_member_count: u64,
    pub membership_count_status: String,
    pub join_conflict: bool,
    pub observed_at_unix_ms: i64,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDetailsRequest {
    pub workflow_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowDetails {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub workflow: WorkflowSummary,
    pub default_model: Option<String>,
    pub script: Option<String>,
    pub script_path: Option<String>,
    pub args: Option<String>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub native_snapshot: Option<JsonValue>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMemberPageRequest {
    pub workflow_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowMemberPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub workflow_id: String,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<WorkflowMember>,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowMember {
    pub member_id: String,
    pub workflow_id: String,
    pub project_id: String,
    pub session_id: String,
    pub child_run_id: String,
    pub child_run_present: bool,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_workflow_id: String,
    pub native_agent_id: String,
    pub native_event_key: String,
    pub native_run_id: Option<String>,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub native_name: Option<String>,
    pub worktree_path: Option<String>,
    pub member_status: String,
    pub result: Option<JsonValue>,
    pub resolution_status: String,
    pub observed_run_state: Option<String>,
    pub delegation_status: Option<String>,
    pub message_count: u64,
    pub decisive_started_fact_id: Option<String>,
    pub decisive_result_fact_id: Option<String>,
    pub started_observed_at_unix_ms: Option<i64>,
    pub result_observed_at_unix_ms: Option<i64>,
    pub started_assertion_count: u64,
    pub competing_started_count: u64,
    pub result_assertion_count: u64,
    pub competing_result_count: u64,
    pub event_key_conflict: bool,
    pub identity_conflict: bool,
    pub source_object_id: u64,
    pub source_generation: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OrchestrationCursorKind {
    Delegations,
    Workflows,
    WorkflowMembers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OrchestrationCursor {
    version: u32,
    kind: OrchestrationCursorKind,
    at_commit_seq: u64,
    scope_hash: String,
    untimed_rank: u8,
    order_text: String,
    entity_key: String,
}

#[derive(Debug)]
struct DelegationRow {
    item: DelegationSummary,
    key: Vec<u8>,
    untimed_rank: u8,
    order_text: String,
}

#[derive(Debug)]
struct WorkflowRow {
    item: WorkflowSummary,
    key: Vec<u8>,
    untimed_rank: u8,
    order_text: String,
}

#[derive(Debug)]
struct WorkflowMemberRow {
    item: WorkflowMember,
    key: Vec<u8>,
    order_text: String,
    payload_bytes: u64,
}

#[derive(Debug)]
struct WorkflowIdentity {
    project_key: Vec<u8>,
    session_key: Vec<u8>,
}

struct WorkflowPagePosition<'a> {
    present: bool,
    untimed_rank: u8,
    order_text: &'a str,
    workflow_key: &'a [u8],
}

pub(super) fn validate_delegation_page(request: &DelegationPageRequest) -> Result<(), EngineError> {
    validate_limit(request.limit, "delegation")?;
    if request.workflow_id.is_some() && request.standalone_only {
        return Err(EngineError::InvalidQuery(
            "delegation workflowId and standaloneOnly cannot be combined".to_string(),
        ));
    }
    let project_key = decode_entity_id(
        &request.project_id,
        PROJECT_ID_PREFIX,
        "delegation project id",
    )?;
    let session_key = decode_entity_id(
        &request.session_id,
        SESSION_ID_PREFIX,
        "delegation session id",
    )?;
    let workflow_key = request
        .workflow_id
        .as_deref()
        .map(|value| decode_entity_id(value, WORKFLOW_ID_PREFIX, "delegation workflow id"))
        .transpose()?;
    let scope_hash = delegation_scope_hash(
        &project_key,
        &session_key,
        workflow_key.as_deref(),
        request.standalone_only,
    );
    decode_optional_cursor(
        request.cursor.as_deref(),
        OrchestrationCursorKind::Delegations,
        &scope_hash,
    )?;
    Ok(())
}

pub(super) fn validate_workflow_page(request: &WorkflowPageRequest) -> Result<(), EngineError> {
    validate_limit(request.limit, "workflow")?;
    let project_key = decode_entity_id(
        &request.project_id,
        PROJECT_ID_PREFIX,
        "workflow project id",
    )?;
    let session_key = decode_entity_id(
        &request.session_id,
        SESSION_ID_PREFIX,
        "workflow session id",
    )?;
    let scope_hash = scope_hash(b"workflow-page-v1", &[&project_key, &session_key], false);
    decode_optional_cursor(
        request.cursor.as_deref(),
        OrchestrationCursorKind::Workflows,
        &scope_hash,
    )?;
    Ok(())
}

pub(super) fn validate_workflow_details(
    request: &WorkflowDetailsRequest,
) -> Result<Vec<u8>, EngineError> {
    decode_entity_id(
        &request.workflow_id,
        WORKFLOW_ID_PREFIX,
        "workflow detail id",
    )
}

pub(super) fn validate_workflow_member_page(
    request: &WorkflowMemberPageRequest,
) -> Result<(), EngineError> {
    validate_limit(request.limit, "workflow member")?;
    let workflow_key = decode_entity_id(
        &request.workflow_id,
        WORKFLOW_ID_PREFIX,
        "workflow member workflow id",
    )?;
    let scope_hash = scope_hash(b"workflow-member-page-v1", &[&workflow_key], false);
    decode_optional_cursor(
        request.cursor.as_deref(),
        OrchestrationCursorKind::WorkflowMembers,
        &scope_hash,
    )?;
    Ok(())
}

pub(super) fn read_delegation_page(
    connection: &Connection,
    request: &DelegationPageRequest,
) -> Result<DelegationPage, EngineError> {
    validate_delegation_page(request)?;
    let project_key = decode_entity_id(
        &request.project_id,
        PROJECT_ID_PREFIX,
        "delegation project id",
    )?;
    let session_key = decode_entity_id(
        &request.session_id,
        SESSION_ID_PREFIX,
        "delegation session id",
    )?;
    let workflow_key = request
        .workflow_id
        .as_deref()
        .map(|value| decode_entity_id(value, WORKFLOW_ID_PREFIX, "delegation workflow id"))
        .transpose()?;
    let scope_hash = delegation_scope_hash(
        &project_key,
        &session_key,
        workflow_key.as_deref(),
        request.standalone_only,
    );
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        OrchestrationCursorKind::Delegations,
        &scope_hash,
    )?;
    let cursor_key = cursor_key(cursor.as_ref())?;
    let cursor_rank = cursor.as_ref().map_or(0, |value| value.untimed_rank);
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());

    let transaction = begin_snapshot(connection, "begin delegation page snapshot")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    require_session_membership(&transaction, &project_key, &session_key, "delegation")?;
    if let Some(workflow_key) = &workflow_key {
        require_workflow_scope(&transaction, workflow_key, &project_key, &session_key)?;
    }
    let mut statement = transaction
        .prepare(
            r#"
            WITH delegation_rows AS (
                SELECT cd.child_run_key, cd.parent_run_key, cs.project_key,
                       cd.session_key, si.adapter_id, fr.source_instance_id,
                       child.native_run_id,
                       COALESCE(cdm.native_child_id, cd.native_child_id),
                       COALESCE(cdm.native_task_id, cd.native_task_id),
                       cdm.agent_type, cdm.description, cdm.native_name,
                       cdm.spawn_depth, COALESCE(cd.label, cds.label),
                       COALESCE(cd.prompt, cds.prompt), cd.cwd,
                       COALESCE(cdm.worktree_path, cd.worktree_path),
                       cd.relation_kind, cd.relation_strength,
                       cd.relation_status, cdm.metadata_status,
                       cds.spawn_status, cds.tool_name,
                       cds.requested_agent_type, anchor.message_key,
                       cd.child_present, cd.parent_present,
                       cdm.run_present, ors.state,
                       (SELECT COUNT(*) FROM canonical_messages cm
                         WHERE cm.run_key = cd.child_run_key),
                       (SELECT COUNT(*) FROM canonical_workflow_members cwm
                         WHERE cwm.child_run_key = cd.child_run_key),
                       cd.source_time, cd.source_time_quality,
                       cd.decisive_relation_fact_id,
                       cd.decisive_spawn_fact_id,
                       cd.decisive_metadata_fact_id,
                       cd.assertion_count, cd.competing_relation_count,
                       fr.observed_at, fr.source_object_id,
                       fr.source_generation, cd.last_commit_seq,
                       CASE WHEN cd.source_time IS NULL THEN 1 ELSE 0 END
                         AS untimed_rank,
                       COALESCE(cd.source_time, '') AS order_text
                FROM canonical_delegations cd
                JOIN canonical_sessions cs ON cs.session_key = cd.session_key
                JOIN fact_records fr
                  ON fr.fact_id = COALESCE(
                    cd.decisive_relation_fact_id,
                    cd.decisive_spawn_fact_id,
                    cd.decisive_metadata_fact_id
                  )
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
                LEFT JOIN canonical_runs child
                  ON child.run_key = cd.child_run_key
                LEFT JOIN observed_run_states ors
                  ON ors.run_key = cd.child_run_key
                LEFT JOIN canonical_delegation_metadata cdm
                  ON cdm.child_run_key = cd.child_run_key
                LEFT JOIN canonical_delegation_spawns cds
                  ON cds.decisive_fact_id = cd.decisive_spawn_fact_id
                LEFT JOIN canonical_messages anchor
                  ON anchor.message_key = cds.parent_message_key
                WHERE cd.session_key = ?1 AND cs.project_key = ?2
                  AND (?3 IS NULL OR EXISTS (
                    SELECT 1 FROM canonical_workflow_members scoped_member
                    WHERE scoped_member.child_run_key = cd.child_run_key
                      AND scoped_member.workflow_key = ?3
                  ))
                  AND (?4 = 0 OR NOT EXISTS (
                    SELECT 1 FROM canonical_workflow_members nested_member
                    WHERE nested_member.child_run_key = cd.child_run_key
                  ))
            )
            SELECT * FROM delegation_rows
            WHERE ?5 = 0
               OR untimed_rank > ?6
               OR (untimed_rank = ?6 AND order_text < ?7)
               OR (untimed_rank = ?6 AND order_text = ?7 AND child_run_key < ?8)
            ORDER BY untimed_rank, order_text DESC, child_run_key DESC
            LIMIT ?9
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare delegation page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            session_key,
            project_key,
            workflow_key,
            i64::from(request.standalone_only),
            i64::from(cursor.is_some()),
            i64::from(cursor_rank),
            cursor_text,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute delegation page", error))?;
    let mut decoded = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance delegation page", error))?
    {
        decoded.push(decode_delegation_row(row)?);
    }
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "finish delegation page snapshot")?;

    let has_more = decoded.len() > request.limit as usize;
    if has_more {
        decoded.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        decoded
            .last()
            .map(|row| {
                encode_cursor(&OrchestrationCursor {
                    version: ORCHESTRATION_QUERY_CONTRACT_VERSION,
                    kind: OrchestrationCursorKind::Delegations,
                    at_commit_seq: watermark,
                    scope_hash: scope_hash.clone(),
                    untimed_rank: row.untimed_rank,
                    order_text: row.order_text.clone(),
                    entity_key: URL_SAFE_NO_PAD.encode(&row.key),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(DelegationPage {
        contract_version: ORCHESTRATION_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        workflow_id: request.workflow_id.clone(),
        standalone_only: request.standalone_only,
        items: decoded.into_iter().map(|row| row.item).collect(),
        next_cursor,
    })
}

pub(super) fn read_workflow_page(
    connection: &Connection,
    request: &WorkflowPageRequest,
) -> Result<WorkflowPage, EngineError> {
    validate_workflow_page(request)?;
    let project_key = decode_entity_id(
        &request.project_id,
        PROJECT_ID_PREFIX,
        "workflow project id",
    )?;
    let session_key = decode_entity_id(
        &request.session_id,
        SESSION_ID_PREFIX,
        "workflow session id",
    )?;
    let scope_hash = scope_hash(b"workflow-page-v1", &[&project_key, &session_key], false);
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        OrchestrationCursorKind::Workflows,
        &scope_hash,
    )?;
    let cursor_key = cursor_key(cursor.as_ref())?;
    let cursor_rank = cursor.as_ref().map_or(0, |value| value.untimed_rank);
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());
    let transaction = begin_snapshot(connection, "begin workflow page snapshot")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    require_session_membership(&transaction, &project_key, &session_key, "workflow")?;
    let mut rows = read_workflow_rows(
        &transaction,
        &project_key,
        &session_key,
        WorkflowPagePosition {
            present: cursor.is_some(),
            untimed_rank: cursor_rank,
            order_text: cursor_text,
            workflow_key: &cursor_key,
        },
        request.limit + 1,
    )?;
    finish_snapshot(transaction, "finish workflow page snapshot")?;
    let has_more = rows.len() > request.limit as usize;
    if has_more {
        rows.truncate(request.limit as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                encode_cursor(&OrchestrationCursor {
                    version: ORCHESTRATION_QUERY_CONTRACT_VERSION,
                    kind: OrchestrationCursorKind::Workflows,
                    at_commit_seq: watermark,
                    scope_hash: scope_hash.clone(),
                    untimed_rank: row.untimed_rank,
                    order_text: row.order_text.clone(),
                    entity_key: URL_SAFE_NO_PAD.encode(&row.key),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(WorkflowPage {
        contract_version: ORCHESTRATION_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        project_id: request.project_id.clone(),
        session_id: request.session_id.clone(),
        items: rows.into_iter().map(|row| row.item).collect(),
        next_cursor,
    })
}

pub(super) fn read_workflow_details(
    connection: &Connection,
    request: &WorkflowDetailsRequest,
) -> Result<WorkflowDetails, EngineError> {
    let workflow_key = validate_workflow_details(request)?;
    let transaction = begin_snapshot(connection, "begin workflow detail snapshot")?;
    let watermark = read_committed_watermark(&transaction)?;
    let workflow = read_workflow_summary(&transaction, &workflow_key)?;
    let detail = transaction
        .query_row(
            r#"
            SELECT default_model, script, script_path, args, summary, error,
                   native_snapshot_json
            FROM canonical_workflows WHERE workflow_key = ?1
            "#,
            [&workflow_key],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                ))
            },
        )
        .map_err(|error| query_sqlite_error("read workflow detail", error))?;
    let payload_bytes = workflow_detail_payload_bytes(
        [
            &detail.0, &detail.1, &detail.2, &detail.3, &detail.4, &detail.5,
        ],
        detail.6.as_deref(),
    )?;
    if payload_bytes > MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES {
        return Err(EngineError::Sqlite {
            operation: "bound workflow detail payload",
            detail: format!(
                "workflow snapshot requires {payload_bytes} bytes; maximum is {MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES}"
            ),
        });
    }
    let native_snapshot = detail
        .6
        .as_deref()
        .map(|bytes| decode_json(bytes, "decode workflow native snapshot"))
        .transpose()?;
    finish_snapshot(transaction, "finish workflow detail snapshot")?;
    Ok(WorkflowDetails {
        contract_version: ORCHESTRATION_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        workflow: workflow.item,
        default_model: detail.0,
        script: detail.1,
        script_path: detail.2,
        args: detail.3,
        summary: detail.4,
        error: detail.5,
        native_snapshot,
        payload_bytes,
        payload_byte_limit: MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES,
    })
}

pub(super) fn read_workflow_member_page(
    connection: &Connection,
    request: &WorkflowMemberPageRequest,
) -> Result<WorkflowMemberPage, EngineError> {
    validate_workflow_member_page(request)?;
    let workflow_key = decode_entity_id(
        &request.workflow_id,
        WORKFLOW_ID_PREFIX,
        "workflow member workflow id",
    )?;
    let scope_hash = scope_hash(b"workflow-member-page-v1", &[&workflow_key], false);
    let cursor = decode_optional_cursor(
        request.cursor.as_deref(),
        OrchestrationCursorKind::WorkflowMembers,
        &scope_hash,
    )?;
    let cursor_key = cursor_key(cursor.as_ref())?;
    let cursor_text = cursor
        .as_ref()
        .map_or("", |value| value.order_text.as_str());
    let transaction = begin_snapshot(connection, "begin workflow member page snapshot")?;
    let watermark = read_committed_watermark(&transaction)?;
    validate_cursor_watermark(cursor.as_ref(), watermark)?;
    let identity = require_workflow(&transaction, &workflow_key)?;
    let mut statement = transaction
        .prepare(
            r#"
            SELECT cwm.member_key, cwm.workflow_key, cwm.project_key,
                   cwm.session_key, cwm.child_run_key,
                   CASE WHEN cr.run_key IS NULL THEN 0 ELSE 1 END,
                   si.adapter_id, provenance.source_instance_id,
                   cwm.native_workflow_id, cwm.native_agent_id,
                   cwm.native_event_key, cr.native_run_id,
                   cdm.agent_type, cdm.description, cdm.native_name,
                   cdm.worktree_path, cwm.member_status, cwm.result_json,
                   cwm.resolution_status, ors.state, cd.relation_status,
                   (SELECT COUNT(*) FROM canonical_messages cm
                     WHERE cm.run_key = cwm.child_run_key),
                   cwm.decisive_started_fact_id,
                   cwm.decisive_result_fact_id,
                   started.observed_at, result.observed_at,
                   cwm.started_assertion_count,
                   cwm.competing_started_count,
                   cwm.result_assertion_count,
                   cwm.competing_result_count,
                   cwm.event_key_conflict, cwm.identity_conflict,
                   provenance.source_object_id,
                   provenance.source_generation, cwm.last_commit_seq
            FROM canonical_workflow_members cwm
            JOIN fact_records provenance
              ON provenance.fact_id = COALESCE(
                cwm.decisive_started_fact_id, cwm.decisive_result_fact_id
              )
            JOIN source_instances si
              ON si.source_instance_id = provenance.source_instance_id
            LEFT JOIN fact_records started
              ON started.fact_id = cwm.decisive_started_fact_id
            LEFT JOIN fact_records result
              ON result.fact_id = cwm.decisive_result_fact_id
            LEFT JOIN canonical_runs cr ON cr.run_key = cwm.child_run_key
            LEFT JOIN observed_run_states ors ON ors.run_key = cwm.child_run_key
            LEFT JOIN canonical_delegations cd
              ON cd.child_run_key = cwm.child_run_key
            LEFT JOIN canonical_delegation_metadata cdm
              ON cdm.child_run_key = cwm.child_run_key
            WHERE cwm.workflow_key = ?1
              AND (?2 = 0 OR cwm.native_agent_id > ?3
                   OR (cwm.native_agent_id = ?3 AND cwm.member_key > ?4))
            ORDER BY cwm.native_agent_id, cwm.member_key
            LIMIT ?5
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare workflow member page", error))?;
    let mut rows = statement
        .query(rusqlite::params![
            workflow_key,
            i64::from(cursor.is_some()),
            cursor_text,
            cursor_key,
            i64::from(request.limit) + 1,
        ])
        .map_err(|error| query_sqlite_error("execute workflow member page", error))?;
    let mut decoded = Vec::new();
    let mut payload_bytes = 0_u64;
    let mut has_more = false;
    while let Some(row) = rows
        .next()
        .map_err(|error| query_sqlite_error("advance workflow member page", error))?
    {
        if decoded.len() == request.limit as usize {
            has_more = true;
            break;
        }
        let member = decode_workflow_member_row(row)?;
        let next_payload = payload_bytes
            .checked_add(member.payload_bytes)
            .ok_or_else(|| EngineError::Sqlite {
                operation: "bound workflow member payload",
                detail: "workflow member payload byte count overflowed u64".to_string(),
            })?;
        if next_payload > MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES {
            if decoded.is_empty() {
                return Err(EngineError::Sqlite {
                    operation: "bound workflow member payload",
                    detail: format!(
                        "one workflow member requires {} payload bytes; maximum is {MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES}",
                        member.payload_bytes
                    ),
                });
            }
            has_more = true;
            break;
        }
        payload_bytes = next_payload;
        decoded.push(member);
    }
    drop(rows);
    drop(statement);
    finish_snapshot(transaction, "finish workflow member page snapshot")?;
    let next_cursor = if has_more {
        decoded
            .last()
            .map(|row| {
                encode_cursor(&OrchestrationCursor {
                    version: ORCHESTRATION_QUERY_CONTRACT_VERSION,
                    kind: OrchestrationCursorKind::WorkflowMembers,
                    at_commit_seq: watermark,
                    scope_hash: scope_hash.clone(),
                    untimed_rank: 0,
                    order_text: row.order_text.clone(),
                    entity_key: URL_SAFE_NO_PAD.encode(&row.key),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(WorkflowMemberPage {
        contract_version: ORCHESTRATION_QUERY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        workflow_id: request.workflow_id.clone(),
        project_id: encode_entity_id(PROJECT_ID_PREFIX, &identity.project_key),
        session_id: encode_entity_id(SESSION_ID_PREFIX, &identity.session_key),
        items: decoded.into_iter().map(|row| row.item).collect(),
        payload_bytes,
        payload_byte_limit: MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES,
        next_cursor,
    })
}

fn decode_delegation_row(row: &Row<'_>) -> Result<DelegationRow, EngineError> {
    let child_run_key: Vec<u8> = query_get(row, 0, "decode delegation child run")?;
    let parent_run_key: Option<Vec<u8>> = query_get(row, 1, "decode delegation parent run")?;
    let project_key: Vec<u8> = query_get(row, 2, "decode delegation project")?;
    let session_key: Vec<u8> = query_get(row, 3, "decode delegation session")?;
    let anchor_key: Option<Vec<u8>> = query_get(row, 24, "decode delegation anchor")?;
    let relation_fact: Option<Vec<u8>> = query_get(row, 33, "decode delegation relation fact")?;
    let spawn_fact: Option<Vec<u8>> = query_get(row, 34, "decode delegation spawn fact")?;
    let metadata_fact: Option<Vec<u8>> = query_get(row, 35, "decode delegation metadata fact")?;
    let untimed_rank = decode_untimed_rank(query_get(row, 42, "decode delegation time rank")?)?;
    let order_text = query_get(row, 43, "decode delegation order time")?;
    Ok(DelegationRow {
        item: DelegationSummary {
            run_id: encode_entity_id(RUN_ID_PREFIX, &child_run_key),
            parent_run_id: parent_run_key
                .as_deref()
                .map(|key| encode_entity_id(RUN_ID_PREFIX, key)),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            adapter_id: query_get(row, 4, "decode delegation adapter")?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 5, "decode delegation source instance")?,
                "delegation source instance",
            )?,
            native_run_id: query_get(row, 6, "decode delegation native run")?,
            native_child_id: query_get(row, 7, "decode delegation native child")?,
            native_task_id: query_get(row, 8, "decode delegation native task")?,
            agent_type: query_get(row, 9, "decode delegation agent type")?,
            description: query_get(row, 10, "decode delegation description")?,
            native_name: query_get(row, 11, "decode delegation native name")?,
            spawn_depth: decode_optional_u32(
                query_get(row, 12, "decode delegation spawn depth")?,
                "delegation spawn depth",
            )?,
            label: query_get(row, 13, "decode delegation label")?,
            prompt: query_get(row, 14, "decode delegation prompt")?,
            cwd: query_get(row, 15, "decode delegation cwd")?,
            worktree_path: query_get(row, 16, "decode delegation worktree")?,
            relation_kind: query_get(row, 17, "decode delegation kind")?,
            relation_strength: query_get(row, 18, "decode delegation strength")?,
            relation_status: query_get(row, 19, "decode delegation status")?,
            metadata_status: query_get(row, 20, "decode delegation metadata status")?,
            spawn_status: query_get(row, 21, "decode delegation spawn status")?,
            branch_tool_name: query_get(row, 22, "decode delegation branch tool")?,
            requested_agent_type: query_get(row, 23, "decode delegation requested agent")?,
            branch_anchor_message_id: anchor_key
                .as_deref()
                .map(|key| encode_entity_id(MESSAGE_ID_PREFIX, key)),
            child_present: decode_bool(query_get(row, 25, "decode delegation child presence")?),
            parent_present: decode_bool(query_get(row, 26, "decode delegation parent presence")?),
            metadata_run_present: query_get::<Option<i64>>(
                row,
                27,
                "decode delegation metadata run presence",
            )?
            .map(decode_bool),
            observed_run_state: query_get(row, 28, "decode delegated run state")?,
            message_count: decode_nonnegative_u64(
                query_get(row, 29, "decode delegated message count")?,
                "delegated message count",
            )?,
            workflow_member_count: decode_nonnegative_u64(
                query_get(row, 30, "decode delegation workflow count")?,
                "delegation workflow member count",
            )?,
            source_time: query_get(row, 31, "decode delegation source time")?,
            source_time_quality: query_get(row, 32, "decode delegation time quality")?,
            decisive_relation_fact_id: relation_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            decisive_spawn_fact_id: spawn_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            decisive_metadata_fact_id: metadata_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            assertion_count: decode_nonnegative_u64(
                query_get(row, 36, "decode delegation assertions")?,
                "delegation assertion count",
            )?,
            competing_relation_count: decode_nonnegative_u64(
                query_get(row, 37, "decode delegation conflicts")?,
                "delegation competing relation count",
            )?,
            observed_at_unix_ms: query_get(row, 38, "decode delegation observation")?,
            source_object_id: decode_nonnegative_u64(
                query_get(row, 39, "decode delegation source object")?,
                "delegation source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 40, "decode delegation source generation")?,
                "delegation source generation",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 41, "decode delegation commit")?,
                "delegation commit sequence",
            )?,
        },
        key: child_run_key,
        untimed_rank,
        order_text,
    })
}

fn read_workflow_rows(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    session_key: &[u8],
    position: WorkflowPagePosition<'_>,
    limit: u32,
) -> Result<Vec<WorkflowRow>, EngineError> {
    let sql = format!(
        "{} WHERE wr.session_key = ?1 AND wr.project_key = ?2\n\
         AND (?3 = 0 OR wr.untimed_rank > ?4\n\
              OR (wr.untimed_rank = ?4 AND wr.order_text < ?5)\n\
              OR (wr.untimed_rank = ?4 AND wr.order_text = ?5 AND wr.workflow_key < ?6))\n\
         ORDER BY wr.untimed_rank, wr.order_text DESC, wr.workflow_key DESC LIMIT ?7",
        workflow_rows_sql()
    );
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|error| query_sqlite_error("prepare workflow page", error))?;
    let rows = statement
        .query_map(
            rusqlite::params![
                session_key,
                project_key,
                i64::from(position.present),
                i64::from(position.untimed_rank),
                position.order_text,
                position.workflow_key,
                i64::from(limit),
            ],
            |row| decode_workflow_row_sql(row).map_err(to_sql_conversion_error),
        )
        .map_err(|error| query_sqlite_error("execute workflow page", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| query_sqlite_error("collect workflow page", error))?;
    Ok(rows)
}

fn read_workflow_summary(
    transaction: &Transaction<'_>,
    workflow_key: &[u8],
) -> Result<WorkflowRow, EngineError> {
    let sql = format!("{} WHERE wr.workflow_key = ?1", workflow_rows_sql());
    transaction
        .query_row(&sql, [workflow_key], |row| {
            decode_workflow_row_sql(row).map_err(to_sql_conversion_error)
        })
        .optional()
        .map_err(|error| query_sqlite_error("read workflow summary", error))?
        .ok_or_else(|| {
            EngineError::InvalidQuery(
                "workflow detail id does not identify a current workflow".to_string(),
            )
        })
}

fn workflow_rows_sql() -> &'static str {
    r#"
    WITH workflow_rows AS (
        SELECT cw.*,
               COALESCE(
                 cw.decisive_snapshot_fact_id,
                 (SELECT COALESCE(member.decisive_started_fact_id,
                                  member.decisive_result_fact_id)
                    FROM canonical_workflow_members member
                   WHERE member.workflow_key = cw.workflow_key
                   ORDER BY member.member_key LIMIT 1)
               ) AS provenance_fact_id,
               CASE WHEN COALESCE(cw.finished_at, cw.started_at) IS NULL
                    THEN 1 ELSE 0 END AS untimed_rank,
               COALESCE(cw.finished_at, cw.started_at, '') AS order_text
        FROM canonical_workflows cw
    )
    SELECT wr.workflow_key, wr.project_key, wr.session_key,
           si.adapter_id, fr.source_instance_id, wr.native_workflow_id,
           wr.native_task_id, wr.name, wr.native_status,
           wr.workflow_status, wr.started_at, wr.started_at_quality,
           wr.finished_at, wr.finished_at_quality, wr.duration_ms,
           wr.agent_count, wr.total_tokens, wr.total_tool_calls,
           wr.snapshot_status, wr.resolution_status,
           wr.decisive_snapshot_fact_id, wr.provenance_fact_id,
           wr.snapshot_assertion_count, wr.competing_snapshot_count,
           wr.observed_member_count, wr.started_member_count,
           wr.result_member_count, wr.unresolved_member_count,
           wr.conflicting_member_count, wr.membership_count_status,
           wr.join_conflict, fr.observed_at, fr.source_object_id,
           fr.source_generation, wr.last_commit_seq, wr.untimed_rank,
           wr.order_text
    FROM workflow_rows wr
    JOIN fact_records fr ON fr.fact_id = wr.provenance_fact_id
    JOIN source_instances si ON si.source_instance_id = fr.source_instance_id
    "#
}

fn decode_workflow_row_sql(row: &Row<'_>) -> Result<WorkflowRow, EngineError> {
    let workflow_key: Vec<u8> = query_get(row, 0, "decode workflow key")?;
    let project_key: Vec<u8> = query_get(row, 1, "decode workflow project")?;
    let session_key: Vec<u8> = query_get(row, 2, "decode workflow session")?;
    let snapshot_fact: Option<Vec<u8>> = query_get(row, 20, "decode workflow snapshot fact")?;
    let provenance_fact: Vec<u8> = query_get(row, 21, "decode workflow provenance fact")?;
    let untimed_rank = decode_untimed_rank(query_get(row, 35, "decode workflow time rank")?)?;
    let order_text = query_get(row, 36, "decode workflow order time")?;
    Ok(WorkflowRow {
        item: WorkflowSummary {
            workflow_id: encode_entity_id(WORKFLOW_ID_PREFIX, &workflow_key),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            adapter_id: query_get(row, 3, "decode workflow adapter")?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 4, "decode workflow source instance")?,
                "workflow source instance",
            )?,
            native_workflow_id: query_get(row, 5, "decode native workflow id")?,
            native_task_id: query_get(row, 6, "decode workflow task id")?,
            name: query_get(row, 7, "decode workflow name")?,
            native_status: query_get(row, 8, "decode workflow native status")?,
            workflow_status: query_get(row, 9, "decode workflow status")?,
            started_at: query_get(row, 10, "decode workflow start")?,
            started_at_quality: query_get(row, 11, "decode workflow start quality")?,
            finished_at: query_get(row, 12, "decode workflow finish")?,
            finished_at_quality: query_get(row, 13, "decode workflow finish quality")?,
            duration_ms: decode_optional_u64(
                query_get(row, 14, "decode workflow duration")?,
                "workflow duration",
            )?,
            agent_count: decode_optional_u64(
                query_get(row, 15, "decode workflow agent count")?,
                "workflow agent count",
            )?,
            total_tokens: decode_optional_u64(
                query_get(row, 16, "decode workflow tokens")?,
                "workflow token count",
            )?,
            total_tool_calls: decode_optional_u64(
                query_get(row, 17, "decode workflow tool calls")?,
                "workflow tool call count",
            )?,
            snapshot_status: query_get(row, 18, "decode workflow snapshot status")?,
            resolution_status: query_get(row, 19, "decode workflow resolution")?,
            decisive_snapshot_fact_id: snapshot_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            provenance_fact_id: encode_entity_id(FACT_ID_PREFIX, &provenance_fact),
            snapshot_assertion_count: decode_nonnegative_u64(
                query_get(row, 22, "decode workflow snapshot assertions")?,
                "workflow snapshot assertion count",
            )?,
            competing_snapshot_count: decode_nonnegative_u64(
                query_get(row, 23, "decode workflow snapshot conflicts")?,
                "workflow competing snapshot count",
            )?,
            observed_member_count: decode_nonnegative_u64(
                query_get(row, 24, "decode workflow member count")?,
                "workflow observed member count",
            )?,
            started_member_count: decode_nonnegative_u64(
                query_get(row, 25, "decode workflow started members")?,
                "workflow started member count",
            )?,
            result_member_count: decode_nonnegative_u64(
                query_get(row, 26, "decode workflow result members")?,
                "workflow result member count",
            )?,
            unresolved_member_count: decode_nonnegative_u64(
                query_get(row, 27, "decode workflow unresolved members")?,
                "workflow unresolved member count",
            )?,
            conflicting_member_count: decode_nonnegative_u64(
                query_get(row, 28, "decode workflow conflicting members")?,
                "workflow conflicting member count",
            )?,
            membership_count_status: query_get(row, 29, "decode workflow count status")?,
            join_conflict: decode_bool(query_get(row, 30, "decode workflow join conflict")?),
            observed_at_unix_ms: query_get(row, 31, "decode workflow observation")?,
            source_object_id: decode_nonnegative_u64(
                query_get(row, 32, "decode workflow source object")?,
                "workflow source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 33, "decode workflow source generation")?,
                "workflow source generation",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 34, "decode workflow commit")?,
                "workflow commit sequence",
            )?,
        },
        key: workflow_key,
        untimed_rank,
        order_text,
    })
}

fn decode_workflow_member_row(row: &Row<'_>) -> Result<WorkflowMemberRow, EngineError> {
    let member_key: Vec<u8> = query_get(row, 0, "decode workflow member key")?;
    let workflow_key: Vec<u8> = query_get(row, 1, "decode member workflow")?;
    let project_key: Vec<u8> = query_get(row, 2, "decode member project")?;
    let session_key: Vec<u8> = query_get(row, 3, "decode member session")?;
    let child_run_key: Vec<u8> = query_get(row, 4, "decode member child run")?;
    let result_json: Option<Vec<u8>> = query_get(row, 17, "decode workflow member result")?;
    let result = result_json
        .as_deref()
        .map(|bytes| decode_json(bytes, "decode workflow member result JSON"))
        .transpose()?;
    let started_fact: Option<Vec<u8>> = query_get(row, 22, "decode member started fact")?;
    let result_fact: Option<Vec<u8>> = query_get(row, 23, "decode member result fact")?;
    let order_text: String = query_get(row, 9, "decode workflow member order")?;
    Ok(WorkflowMemberRow {
        item: WorkflowMember {
            member_id: encode_entity_id(WORKFLOW_MEMBER_ID_PREFIX, &member_key),
            workflow_id: encode_entity_id(WORKFLOW_ID_PREFIX, &workflow_key),
            project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
            session_id: encode_entity_id(SESSION_ID_PREFIX, &session_key),
            child_run_id: encode_entity_id(RUN_ID_PREFIX, &child_run_key),
            child_run_present: decode_bool(query_get(row, 5, "decode member run presence")?),
            adapter_id: query_get(row, 6, "decode workflow member adapter")?,
            source_instance_id: decode_nonnegative_u64(
                query_get(row, 7, "decode workflow member source instance")?,
                "workflow member source instance",
            )?,
            native_workflow_id: query_get(row, 8, "decode member native workflow")?,
            native_agent_id: order_text.clone(),
            native_event_key: query_get(row, 10, "decode member event key")?,
            native_run_id: query_get(row, 11, "decode member native run")?,
            agent_type: query_get(row, 12, "decode member agent type")?,
            description: query_get(row, 13, "decode member description")?,
            native_name: query_get(row, 14, "decode member native name")?,
            worktree_path: query_get(row, 15, "decode member worktree")?,
            member_status: query_get(row, 16, "decode member status")?,
            result,
            resolution_status: query_get(row, 18, "decode member resolution")?,
            observed_run_state: query_get(row, 19, "decode member run state")?,
            delegation_status: query_get(row, 20, "decode member delegation status")?,
            message_count: decode_nonnegative_u64(
                query_get(row, 21, "decode member message count")?,
                "workflow member message count",
            )?,
            decisive_started_fact_id: started_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            decisive_result_fact_id: result_fact
                .as_deref()
                .map(|key| encode_entity_id(FACT_ID_PREFIX, key)),
            started_observed_at_unix_ms: query_get(row, 24, "decode member start observation")?,
            result_observed_at_unix_ms: query_get(row, 25, "decode member result observation")?,
            started_assertion_count: decode_nonnegative_u64(
                query_get(row, 26, "decode member started assertions")?,
                "workflow member started assertion count",
            )?,
            competing_started_count: decode_nonnegative_u64(
                query_get(row, 27, "decode member started conflicts")?,
                "workflow member competing started count",
            )?,
            result_assertion_count: decode_nonnegative_u64(
                query_get(row, 28, "decode member result assertions")?,
                "workflow member result assertion count",
            )?,
            competing_result_count: decode_nonnegative_u64(
                query_get(row, 29, "decode member result conflicts")?,
                "workflow member competing result count",
            )?,
            event_key_conflict: decode_bool(query_get(row, 30, "decode member event conflict")?),
            identity_conflict: decode_bool(query_get(row, 31, "decode member identity conflict")?),
            source_object_id: decode_nonnegative_u64(
                query_get(row, 32, "decode member source object")?,
                "workflow member source object",
            )?,
            source_generation: decode_nonnegative_u64(
                query_get(row, 33, "decode member source generation")?,
                "workflow member source generation",
            )?,
            last_commit_seq: decode_nonnegative_u64(
                query_get(row, 34, "decode member commit")?,
                "workflow member commit sequence",
            )?,
        },
        key: member_key,
        order_text,
        payload_bytes: result_json.as_ref().map_or(Ok(0), |bytes| {
            usize_to_u64(bytes.len(), "workflow member result length")
        })?,
    })
}

fn require_session_membership(
    transaction: &Transaction<'_>,
    project_key: &[u8],
    session_key: &[u8],
    label: &str,
) -> Result<(), EngineError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM canonical_sessions WHERE session_key = ?1 AND project_key = ?2",
            rusqlite::params![session_key, project_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("verify orchestration session membership", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(format!(
            "{label} projectId/sessionId does not identify a current canonical session"
        )));
    }
    Ok(())
}

fn require_workflow_scope(
    transaction: &Transaction<'_>,
    workflow_key: &[u8],
    project_key: &[u8],
    session_key: &[u8],
) -> Result<(), EngineError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM canonical_workflows WHERE workflow_key = ?1 AND project_key = ?2 AND session_key = ?3",
            rusqlite::params![workflow_key, project_key, session_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| query_sqlite_error("verify delegation workflow scope", error))?
        .is_some();
    if !exists {
        return Err(EngineError::InvalidQuery(
            "delegation workflowId does not belong to the requested project/session".to_string(),
        ));
    }
    Ok(())
}

fn require_workflow(
    transaction: &Transaction<'_>,
    workflow_key: &[u8],
) -> Result<WorkflowIdentity, EngineError> {
    transaction
        .query_row(
            "SELECT project_key, session_key FROM canonical_workflows WHERE workflow_key = ?1",
            [workflow_key],
            |row| {
                Ok(WorkflowIdentity {
                    project_key: row.get(0)?,
                    session_key: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|error| query_sqlite_error("verify current workflow", error))?
        .ok_or_else(|| {
            EngineError::InvalidQuery(
                "workflow member workflow id does not identify a current workflow".to_string(),
            )
        })
}

fn delegation_scope_hash(
    project_key: &[u8],
    session_key: &[u8],
    workflow_key: Option<&[u8]>,
    standalone_only: bool,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_component(&mut hasher, b"delegation-page-v1");
    hash_component(&mut hasher, project_key);
    hash_component(&mut hasher, session_key);
    match workflow_key {
        Some(key) => {
            hasher.update(&[1]);
            hash_component(&mut hasher, key);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&[u8::from(standalone_only)]);
    URL_SAFE_NO_PAD.encode(hasher.finalize().as_bytes())
}

fn scope_hash(domain: &[u8], components: &[&[u8]], flag: bool) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_component(&mut hasher, domain);
    for component in components {
        hash_component(&mut hasher, component);
    }
    hasher.update(&[u8::from(flag)]);
    URL_SAFE_NO_PAD.encode(hasher.finalize().as_bytes())
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn validate_limit(limit: u32, label: &str) -> Result<(), EngineError> {
    if !(1..=MAX_ORCHESTRATION_PAGE_LIMIT).contains(&limit) {
        return Err(EngineError::InvalidQuery(format!(
            "{label} page limit must be between 1 and {MAX_ORCHESTRATION_PAGE_LIMIT}, got {limit}"
        )));
    }
    Ok(())
}

fn decode_optional_cursor(
    value: Option<&str>,
    kind: OrchestrationCursorKind,
    scope_hash: &str,
) -> Result<Option<OrchestrationCursor>, EngineError> {
    value
        .map(|value| decode_cursor(value, kind, scope_hash))
        .transpose()
}

fn decode_cursor(
    value: &str,
    kind: OrchestrationCursorKind,
    scope_hash: &str,
) -> Result<OrchestrationCursor, EngineError> {
    if value.is_empty() || value.len() > MAX_ORCHESTRATION_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "orchestration cursor is empty or exceeds the supported bound".to_string(),
        ));
    }
    let json = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("orchestration cursor is not valid base64url".to_string())
    })?;
    let cursor: OrchestrationCursor = serde_json::from_slice(&json).map_err(|_| {
        EngineError::InvalidQuery("orchestration cursor payload is malformed".to_string())
    })?;
    if cursor.version != ORCHESTRATION_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported orchestration cursor version {}",
            cursor.version
        )));
    }
    if cursor.kind != kind || cursor.scope_hash != scope_hash {
        return Err(EngineError::InvalidQuery(
            "orchestration cursor does not belong to this query".to_string(),
        ));
    }
    if cursor.untimed_rank > 1 || cursor.order_text.len() > MAX_ORCHESTRATION_CURSOR_BYTES {
        return Err(EngineError::InvalidQuery(
            "orchestration cursor order key exceeds the supported bound".to_string(),
        ));
    }
    cursor_key(Some(&cursor))?;
    Ok(cursor)
}

fn cursor_key(cursor: Option<&OrchestrationCursor>) -> Result<Vec<u8>, EngineError> {
    cursor
        .map(|cursor| {
            let key = URL_SAFE_NO_PAD.decode(&cursor.entity_key).map_err(|_| {
                EngineError::InvalidQuery(
                    "orchestration cursor entity key is malformed".to_string(),
                )
            })?;
            if key.is_empty() || key.len() > MAX_ORCHESTRATION_CURSOR_BYTES {
                return Err(EngineError::InvalidQuery(
                    "orchestration cursor entity key is outside the supported bound".to_string(),
                ));
            }
            Ok(key)
        })
        .transpose()
        .map(|value| value.unwrap_or_default())
}

fn encode_cursor(cursor: &OrchestrationCursor) -> Result<String, EngineError> {
    let json = serde_json::to_vec(cursor).map_err(|error| EngineError::Sqlite {
        operation: "encode orchestration cursor",
        detail: error.to_string(),
    })?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn validate_cursor_watermark(
    cursor: Option<&OrchestrationCursor>,
    watermark: u64,
) -> Result<(), EngineError> {
    if let Some(cursor) = cursor {
        if cursor.at_commit_seq != watermark {
            return Err(EngineError::InvalidQuery(format!(
                "orchestration cursor expired at commit {}; current commit is {watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    Ok(())
}

fn begin_snapshot<'a>(
    connection: &'a Connection,
    operation: &'static str,
) -> Result<Transaction<'a>, EngineError> {
    connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error(operation, error))
}

fn finish_snapshot(
    transaction: Transaction<'_>,
    operation: &'static str,
) -> Result<(), EngineError> {
    transaction
        .commit()
        .map_err(|error| query_sqlite_error(operation, error))
}

fn decode_json(bytes: &[u8], operation: &'static str) -> Result<JsonValue, EngineError> {
    serde_json::from_slice(bytes).map_err(|error| EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    })
}

fn decode_bool(value: i64) -> bool {
    value != 0
}

fn decode_untimed_rank(value: i64) -> Result<u8, EngineError> {
    let value = u8::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode orchestration time rank",
        detail: format!("time rank was outside u8: {value}"),
    })?;
    if value > 1 {
        return Err(EngineError::Sqlite {
            operation: "decode orchestration time rank",
            detail: format!("time rank was not zero or one: {value}"),
        });
    }
    Ok(value)
}

fn decode_nonnegative_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode orchestration integer",
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

fn decode_optional_u32(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<u32>, EngineError> {
    value
        .map(|value| {
            u32::try_from(value).map_err(|_| EngineError::Sqlite {
                operation: "decode orchestration integer",
                detail: format!("{field} was outside u32: {value}"),
            })
        })
        .transpose()
}

fn usize_to_u64(value: usize, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode orchestration payload",
        detail: format!("{field} exceeded u64"),
    })
}

fn workflow_detail_payload_bytes(
    text: [&Option<String>; 6],
    snapshot: Option<&[u8]>,
) -> Result<u64, EngineError> {
    let mut total = snapshot.map_or(0, <[u8]>::len);
    for value in text.into_iter().flatten() {
        total = total
            .checked_add(value.len())
            .ok_or_else(|| EngineError::Sqlite {
                operation: "bound workflow detail payload",
                detail: "workflow detail payload byte count overflowed usize".to_string(),
            })?;
    }
    usize_to_u64(total, "workflow detail length")
}

fn query_get<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    index: usize,
    operation: &'static str,
) -> Result<T, EngineError> {
    row.get(index)
        .map_err(|error| query_sqlite_error(operation, error))
}

fn to_sql_conversion_error(error: EngineError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Null, Box::new(error))
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
        fact_kind: &str,
        entity_key: &[u8],
        object_id: i64,
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
                ) VALUES (?1, ?2, ?3, 1, 1, ?4, 1, x'00', x'01', x'AA',
                          0, ?5, x'7B7D', 1)
                "#,
                params![fact_id, fact_kind, entity_key, object_id, observed_at],
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
                    (1, 'fixture', x'01', 'Fixture', '1.0.0', 1, '[]', '[]', 1, 1);
                INSERT INTO source_streams (
                    source_stream_id, source_instance_id, stream_key,
                    driver_kind, decoder_key, stream_state, last_commit_seq
                ) VALUES
                    (1, 1, 'fixture', 'append_file', 'fixture', 'available', 1);
                INSERT INTO source_objects (
                    source_object_id, source_stream_id, object_key,
                    generation, committed_cursor, decoder_contract_version,
                    last_commit_seq, state
                ) VALUES
                    (1, 1, x'01', 1, x'01', 1, 1, 'active'),
                    (2, 1, x'02', 1, x'01', 1, 1, 'active'),
                    (3, 1, x'03', 1, x'01', 1, 1, 'active');
                INSERT INTO ingest_commits
                    (commit_seq, source_instance_id, reason, started_at,
                     committed_at, fact_count)
                VALUES (1, 1, 'seed', 1, 2, 10);
                "#,
            )
            .unwrap();
        insert_fact(&connection, b"fs", "session", b"s1", 1, 100);
        insert_fact(&connection, b"fr", "run", b"r1", 1, 101);
        insert_fact(&connection, b"fc", "run", b"rc", 1, 102);
        insert_fact(&connection, b"fd", "delegation", b"rc", 1, 103);
        insert_fact(&connection, b"fm", "delegation_metadata", b"rc", 2, 104);
        insert_fact(&connection, b"fw", "workflow_snapshot", b"w1", 2, 105);
        insert_fact(&connection, b"fws", "workflow_member_event", b"wm1", 3, 106);
        insert_fact(&connection, b"fwr", "workflow_member_event", b"wm1", 3, 107);
        insert_fact(&connection, b"fwo", "workflow_member_event", b"wm2", 3, 108);
        connection
            .execute_batch(
                r#"
                INSERT INTO canonical_sessions (
                    session_key, project_key, native_session_id,
                    native_project_key, fact_id, source_object_id,
                    source_generation, cursor_end, last_commit_seq
                ) VALUES (x'7331', x'7031', 'native-s1', 'native-p1',
                          x'6673', 1, 1, x'01', 1);
                INSERT INTO canonical_runs (
                    run_key, session_key, native_run_id, parent_run_key,
                    fact_id, source_object_id, source_generation, cursor_end,
                    last_commit_seq
                ) VALUES
                    (x'7231', x'7331', 'root', NULL, x'6672', 1, 1, x'01', 1),
                    (x'7263', x'7331', 'child', x'7231', x'6663', 1, 1, x'01', 1);
                INSERT INTO delegation_assertions (
                    fact_id, child_run_key, parent_run_key, session_key,
                    relation_kind, relation_strength, native_child_id,
                    native_task_id, label, prompt, cwd, worktree_path,
                    source_time, source_time_quality, source_object_id,
                    source_generation, cursor_end, last_commit_seq
                ) VALUES (
                    x'6664', x'7263', x'7231', x'7331',
                    'vendor_native_subagent', 'layout', 'agent-one', NULL,
                    'helper', 'do work', '/repo', '/repo/.worktrees/agent-one',
                    '2026-08-12T00:00:00.000Z', 'native_exact', 1, 1, x'01', 1
                );
                INSERT INTO canonical_delegations (
                    child_run_key, parent_run_key, session_key, relation_kind,
                    relation_strength, relation_status, native_child_id,
                    native_task_id, label, prompt, cwd, worktree_path,
                    source_time, source_time_quality,
                    decisive_relation_fact_id, decisive_spawn_fact_id,
                    decisive_metadata_fact_id, assertion_count,
                    competing_relation_count, child_present, parent_present,
                    last_commit_seq
                ) VALUES (
                    x'7263', x'7231', x'7331', 'vendor_native_subagent',
                    'layout', 'resolved', 'agent-one', NULL, 'helper',
                    'do work', '/repo', '/repo/.worktrees/agent-one',
                    '2026-08-12T00:00:00.000Z', 'native_exact', x'6664',
                    NULL, NULL, 1, 0, 1, 1, 1
                );
                INSERT INTO delegation_metadata_assertions (
                    fact_id, child_run_key, session_key, native_child_id,
                    agent_type, description, native_name, spawn_depth,
                    worktree_path, native_task_id, source_object_id,
                    source_generation, cursor_end, last_commit_seq
                ) VALUES (
                    x'666d', x'7263', x'7331', 'agent-one', 'Explore',
                    'investigate', 'worker', 1, '/repo/.worktrees/agent-one',
                    NULL, 2, 1, x'01', 1
                );
                INSERT INTO canonical_delegation_metadata (
                    child_run_key, session_key, native_child_id, agent_type,
                    description, native_name, spawn_depth, worktree_path,
                    native_task_id, metadata_status, decisive_fact_id,
                    assertion_count, competing_metadata_count, run_present,
                    last_commit_seq
                ) VALUES (
                    x'7263', x'7331', 'agent-one', 'Explore', 'investigate',
                    'worker', 1, '/repo/.worktrees/agent-one', NULL, 'resolved',
                    x'666d', 1, 0, 1, 1
                );
                INSERT INTO workflow_snapshot_assertions (
                    fact_id, workflow_key, session_key, project_key,
                    native_workflow_id, native_task_id, name, native_status,
                    workflow_status, default_model, script, script_path,
                    args, summary, error, started_at, started_at_quality,
                    finished_at, finished_at_quality, duration_ms, agent_count,
                    total_tokens, total_tool_calls, native_snapshot_json,
                    snapshot_digest, source_object_id, source_generation,
                    cursor_end, last_commit_seq
                ) VALUES (
                    x'6677', x'7731', x'7331', x'7031', 'wf-one', 'task-one',
                    'audit', 'completed', 'succeeded', 'model', 'run()',
                    '/repo/run.js', '--fast', 'done', NULL,
                    '2026-08-12T00:00:00.000Z', 'native_exact',
                    '2026-08-12T00:00:05.000Z', 'native_exact', 5000, 2,
                    99, 7, x'7B2272756E4964223A2277662D6F6E65227D', x'AA',
                    2, 1, x'01', 1
                );
                INSERT INTO workflow_member_event_assertions (
                    fact_id, workflow_key, member_key, child_run_key,
                    session_key, project_key, native_workflow_id,
                    native_agent_id, native_event_key, event_kind, result_json,
                    event_digest, source_object_id, source_generation,
                    cursor_end, last_commit_seq
                ) VALUES
                    (x'667773', x'7731', x'776d31', x'7263', x'7331', x'7031',
                     'wf-one', 'agent-one', 'event-one', 'started', NULL, x'01',
                     3, 1, x'01', 1),
                    (x'667772', x'7731', x'776d31', x'7263', x'7331', x'7031',
                     'wf-one', 'agent-one', 'event-one', 'result',
                     x'7B2273756D6D617279223A22646F6E65227D', x'02', 3, 1, x'01', 1),
                    (x'66776f', x'7731', x'776d32', x'726f', x'7331', x'7031',
                     'wf-one', 'agent-two', 'event-two', 'started', NULL, x'03',
                     3, 1, x'01', 1);
                INSERT INTO canonical_workflow_members (
                    member_key, workflow_key, child_run_key, session_key,
                    project_key, native_workflow_id, native_agent_id,
                    native_event_key, member_status, result_json,
                    resolution_status, decisive_started_fact_id,
                    decisive_result_fact_id, started_assertion_count,
                    competing_started_count, result_assertion_count,
                    competing_result_count, event_key_conflict,
                    identity_conflict, last_commit_seq
                ) VALUES
                    (x'776d31', x'7731', x'7263', x'7331', x'7031', 'wf-one',
                     'agent-one', 'event-one', 'result_observed',
                     x'7B2273756D6D617279223A22646F6E65227D', 'resolved',
                     x'667773', x'667772', 1, 0, 1, 0, 0, 0, 1),
                    (x'776d32', x'7731', x'726f', x'7331', x'7031', 'wf-one',
                     'agent-two', 'event-two', 'started', NULL, 'resolved',
                     x'66776f', NULL, 1, 0, 0, 0, 0, 0, 1);
                INSERT INTO canonical_workflows (
                    workflow_key, session_key, project_key, native_workflow_id,
                    native_task_id, name, native_status, workflow_status,
                    default_model, script, script_path, args, summary, error,
                    started_at, started_at_quality, finished_at,
                    finished_at_quality, duration_ms, agent_count, total_tokens,
                    total_tool_calls, native_snapshot_json, snapshot_status,
                    resolution_status, decisive_snapshot_fact_id,
                    snapshot_assertion_count, competing_snapshot_count,
                    observed_member_count, started_member_count,
                    result_member_count, unresolved_member_count,
                    conflicting_member_count, membership_count_status,
                    join_conflict, last_commit_seq
                ) VALUES (
                    x'7731', x'7331', x'7031', 'wf-one', 'task-one', 'audit',
                    'completed', 'succeeded', 'model', 'run()', '/repo/run.js',
                    '--fast', 'done', NULL, '2026-08-12T00:00:00.000Z',
                    'native_exact', '2026-08-12T00:00:05.000Z', 'native_exact',
                    5000, 2, 99, 7, x'7B2272756E4964223A2277662D6F6E65227D',
                    'present', 'resolved', x'6677', 1, 0, 2, 2, 1, 1, 0,
                    'matched', 0, 1
                );
                "#,
            )
            .unwrap();
        connection
    }

    fn delegation_request(limit: u32) -> DelegationPageRequest {
        DelegationPageRequest {
            project_id: encode_entity_id(PROJECT_ID_PREFIX, b"p1"),
            session_id: encode_entity_id(SESSION_ID_PREFIX, b"s1"),
            workflow_id: None,
            standalone_only: false,
            cursor: None,
            limit,
        }
    }

    fn workflow_request(limit: u32) -> WorkflowPageRequest {
        WorkflowPageRequest {
            project_id: encode_entity_id(PROJECT_ID_PREFIX, b"p1"),
            session_id: encode_entity_id(SESSION_ID_PREFIX, b"s1"),
            cursor: None,
            limit,
        }
    }

    #[test]
    fn delegation_discovery_uses_current_relations_and_workflow_membership() {
        let connection = seeded_connection();
        let page = read_delegation_page(&connection, &delegation_request(10)).unwrap();
        assert_eq!(page.contract_version, 1);
        assert_eq!(page.items.len(), 1);
        let item = &page.items[0];
        assert_eq!(item.native_child_id.as_deref(), Some("agent-one"));
        assert_eq!(item.agent_type.as_deref(), Some("Explore"));
        assert_eq!(item.native_name.as_deref(), Some("worker"));
        assert_eq!(item.message_count, 0);
        assert_eq!(item.workflow_member_count, 1);
        assert_eq!(item.relation_status, "resolved");
        assert!(item.child_present);
        assert!(item.parent_present);

        let mut workflow = delegation_request(10);
        workflow.workflow_id = Some(encode_entity_id(WORKFLOW_ID_PREFIX, b"w1"));
        assert_eq!(
            read_delegation_page(&connection, &workflow)
                .unwrap()
                .items
                .len(),
            1
        );
        let mut standalone = delegation_request(10);
        standalone.standalone_only = true;
        assert!(read_delegation_page(&connection, &standalone)
            .unwrap()
            .items
            .is_empty());
    }

    #[test]
    fn workflows_and_members_preserve_snapshot_result_and_missing_child() {
        let connection = seeded_connection();
        let workflows = read_workflow_page(&connection, &workflow_request(10)).unwrap();
        assert_eq!(workflows.items.len(), 1);
        let workflow = &workflows.items[0];
        assert_eq!(workflow.native_workflow_id, "wf-one");
        assert_eq!(workflow.workflow_status.as_deref(), Some("succeeded"));
        assert_eq!(workflow.observed_member_count, 2);
        assert_eq!(workflow.result_member_count, 1);
        assert_eq!(workflow.unresolved_member_count, 1);

        let details = read_workflow_details(
            &connection,
            &WorkflowDetailsRequest {
                workflow_id: workflow.workflow_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(details.default_model.as_deref(), Some("model"));
        assert_eq!(
            details.native_snapshot,
            Some(serde_json::json!({ "runId": "wf-one" }))
        );
        assert!(details.payload_bytes > 0);

        let first = read_workflow_member_page(
            &connection,
            &WorkflowMemberPageRequest {
                workflow_id: workflow.workflow_id.clone(),
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(first.items[0].native_agent_id, "agent-one");
        assert_eq!(first.items[0].member_status, "result_observed");
        assert_eq!(
            first.items[0].result,
            Some(serde_json::json!({ "summary": "done" }))
        );
        assert!(first.items[0].child_run_present);
        assert!(first.next_cursor.is_some());
        let second = read_workflow_member_page(
            &connection,
            &WorkflowMemberPageRequest {
                workflow_id: workflow.workflow_id.clone(),
                cursor: first.next_cursor,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(second.items[0].native_agent_id, "agent-two");
        assert_eq!(second.items[0].member_status, "started");
        assert!(!second.items[0].child_run_present);
        assert_eq!(second.items[0].observed_run_state, None);
    }

    #[test]
    fn orchestration_cursors_are_scope_and_watermark_bound() {
        let connection = seeded_connection();
        let first = read_workflow_member_page(
            &connection,
            &WorkflowMemberPageRequest {
                workflow_id: encode_entity_id(WORKFLOW_ID_PREFIX, b"w1"),
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
        let cursor = first.next_cursor.unwrap();
        let mut delegation = delegation_request(1);
        delegation.cursor = Some(cursor.clone());
        assert!(matches!(
            read_delegation_page(&connection, &delegation),
            Err(EngineError::InvalidQuery(_))
        ));

        connection
            .execute(
                "INSERT INTO ingest_commits VALUES (2, 1, 'later', 3, 4, 0)",
                [],
            )
            .unwrap();
        let error = read_workflow_member_page(
            &connection,
            &WorkflowMemberPageRequest {
                workflow_id: encode_entity_id(WORKFLOW_ID_PREFIX, b"w1"),
                cursor: Some(cursor),
                limit: 1,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("expired"));
    }

    #[test]
    fn orchestration_queries_validate_scope_and_remain_read_only() {
        let connection = seeded_connection();
        let before: i64 = connection
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();
        read_delegation_page(&connection, &delegation_request(10)).unwrap();
        read_workflow_page(&connection, &workflow_request(10)).unwrap();
        let after: i64 = connection
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, before);

        let mut invalid = delegation_request(0);
        assert!(matches!(
            read_delegation_page(&connection, &invalid),
            Err(EngineError::InvalidQuery(_))
        ));
        invalid.limit = 10;
        invalid.workflow_id = Some(encode_entity_id(WORKFLOW_ID_PREFIX, b"w1"));
        invalid.standalone_only = true;
        assert!(matches!(
            read_delegation_page(&connection, &invalid),
            Err(EngineError::InvalidQuery(_))
        ));
        let mut mismatch = workflow_request(10);
        mismatch.project_id = encode_entity_id(PROJECT_ID_PREFIX, b"other");
        assert!(matches!(
            read_workflow_page(&connection, &mismatch),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn orchestration_keyset_queries_use_their_ordering_indexes() {
        let connection = seeded_connection();
        let cases = [
            (
                "SELECT child_run_key FROM canonical_delegations \
                 WHERE session_key = ?1 \
                 ORDER BY CASE WHEN source_time IS NULL THEN 1 ELSE 0 END, \
                          COALESCE(source_time, '') DESC, child_run_key DESC \
                 LIMIT 10",
                "idx_canonical_delegations_session_activity",
            ),
            (
                "SELECT workflow_key FROM canonical_workflows \
                 WHERE session_key = ?1 AND project_key = ?2 \
                 ORDER BY CASE WHEN COALESCE(finished_at, started_at) IS NULL \
                               THEN 1 ELSE 0 END, \
                          COALESCE(finished_at, started_at, '') DESC, \
                          workflow_key DESC LIMIT 10",
                "idx_canonical_workflows_session_activity",
            ),
            (
                "SELECT member_key FROM canonical_workflow_members \
                 WHERE workflow_key = ?1 \
                 ORDER BY native_agent_id, member_key LIMIT 10",
                "idx_canonical_workflow_members_workflow_order",
            ),
        ];
        for (sql, expected_index) in cases {
            let parameter_count = connection.prepare(sql).unwrap().parameter_count();
            let plan = match parameter_count {
                1 => connection
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .unwrap()
                    .query_map([b"scope".as_slice()], |row| row.get::<_, String>(3))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                2 => connection
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .unwrap()
                    .query_map(
                        rusqlite::params![b"scope".as_slice(), b"project".as_slice()],
                        |row| row.get::<_, String>(3),
                    )
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                count => panic!("unexpected orchestration plan parameter count: {count}"),
            };
            assert!(
                plan.iter().any(|step| step.contains(expected_index)),
                "expected {expected_index} in query plan: {plan:?}"
            );
        }
    }
}
