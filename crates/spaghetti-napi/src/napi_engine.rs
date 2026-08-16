//! N-API host adapter for the library-first persistent engine.

use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use napi::bindgen_prelude::{
    AbortSignal, AsyncBlock, AsyncBlockBuilder, AsyncTask, Env, Error, Result, Status, Task,
};
use napi_derive::napi;

use crate::adapter::AdapterRegistry;
use crate::claude::ClaudeCodeAdapter;
use crate::codex::CodexAdapter;
use crate::engine::{
    ArtifactDetail, ArtifactPage, ArtifactPageRequest, CanonicalStats, ChangeCursor, ChangeReplay,
    ChangeReplayRequest, CheckpointPerformanceSnapshot, CommitWaitResult, DelegationPage,
    DelegationPageRequest, DelegationSummary, DurableChange, EngineError, EngineHealthSnapshot,
    EngineOptions, EngineOverview, EngineStatusSnapshot, FactFamilyCoverageItem,
    FactFamilyCoveragePage, FactFamilyCoveragePageRequest, FactFamilyCoverageSetSummary,
    HistoryProjectIndexSummary, HistoryProjectPage, HistoryProjectPageRequest,
    HistoryProjectSummary, HistorySessionIndexSummary, HistorySessionPage,
    HistorySessionPageRequest, HistorySessionSummary, MemoryDocument, MemoryDocumentPage,
    MemoryDocumentPageRequest, MessageDetail, MessagePage, MessagePageRequest, NamedCount,
    NamedLatencySnapshot, ObservationStatusSnapshot, ObservationSupervisorOptions, OwnerMetadata,
    PlanDetail, PlanPage, PlanPageRequest, QueryCancellationToken, QueryPerformanceSnapshot,
    ReconcileOutcome, ReconcileRequest, RunStateLookup, RunStateRequest, RuntimePresenceSnapshot,
    RuntimeRunEvidence, RuntimeRunSnapshot, RuntimeSnapshot, RuntimeSnapshotRequest,
    RuntimeUsageV2ActorContext, RuntimeUsageV2Affiliation, RuntimeUsageV2Aggregate,
    RuntimeUsageV2BucketAggregate, RuntimeUsageV2ExternalEntityRef, RuntimeUsageV2Page,
    RuntimeUsageV2PageRequest, RuntimeUsageV2ProjectionReadiness, RuntimeUsageV2Response,
    RuntimeUsageV2SemanticRevisionRef, RuntimeUsageV2TextValue, RuntimeUsageV2TokenValue,
    RuntimeUsageV2ValueProvenance, SearchHit, SearchPage, SearchPageRequest, SessionDetail,
    SessionDetails, SessionDetailsRequest, SessionIndexDetail, SourceCapabilitySummary,
    SourceDimensionPerformanceSnapshot, SourcePage, SourcePageRequest, SourcePerformanceSnapshot,
    SourcePipelineSnapshot, SourceSummary, SpaghettiEngineCore, StoragePerformanceSnapshot,
    TaskCollectionPage, TaskCollectionPageRequest, TaskCollectionSummary, TaskDetail, TaskPage,
    TaskPageRequest, TeamConfigSummary, TeamDetails, TeamDetailsRequest, TeamInboxMessage,
    TeamInboxMessagePage, TeamInboxMessagePageRequest, TeamInboxPage, TeamInboxPageRequest,
    TeamInboxSummary, TeamMember, TeamPage, TeamPageRequest, TeamSummary, TimelineFacets,
    TimelineMessage, TimelinePage, TimelinePageRequest, ToolResultDetail, ToolResultPage,
    ToolResultPageRequest, UntimedUsageSummary, UsageActivityDay, UsageActivityReport,
    UsageActivityRequest, UsageAggregate, UsageCoverageSummary, UsageScopeRequest,
    UsageTokenValues, UsageTotalsReport, WorkflowDetails, WorkflowDetailsRequest, WorkflowMember,
    WorkflowMemberPage, WorkflowMemberPageRequest, WorkflowPage, WorkflowPageRequest,
    WorkflowSummary, WriterPerformanceSnapshot, CHANGE_REPLAY_CONTRACT_VERSION,
    DEFAULT_CAPABILITY_PAGE_LIMIT, DEFAULT_CHANGE_REPLAY_LIMIT, DEFAULT_COMMIT_WAIT_TIMEOUT_MS,
    DEFAULT_DETAIL_PAGE_LIMIT, DEFAULT_FACT_FAMILY_COVERAGE_PAGE_LIMIT, DEFAULT_HISTORY_PAGE_LIMIT,
    DEFAULT_ORCHESTRATION_PAGE_LIMIT, DEFAULT_RUNTIME_PAGE_LIMIT,
    DEFAULT_RUNTIME_USAGE_V2_PAGE_LIMIT, DEFAULT_SEARCH_PAGE_LIMIT, DEFAULT_TEAM_PAGE_LIMIT,
    DEFAULT_TIMELINE_PAGE_LIMIT, MAX_CHANGE_REPLAY_PAYLOAD_BYTES,
};
use crate::grok::GrokAdapter;

const CLAUDE_ADAPTER_ID: &str = "claude-code";

fn ns_to_ms(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineOpenOptions {
    /// Canonical SQLite database owned by this engine instance.
    pub db_path: String,
    /// Number of persistent read-only workers. Defaults to 2; maximum 16.
    pub query_workers: Option<u32>,
    /// Diagnostic host label persisted in the owner metadata sidecar.
    pub owner_label: Option<String>,
    /// Defer reviewed query-only structures for one large fresh bootstrap.
    pub bootstrap_query_structures: Option<bool>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineOwnerMetadata {
    pub protocol_version: u32,
    pub owner_id: String,
    pub owner_label: String,
    pub process_id: u32,
    pub started_at_unix_ms: f64,
    pub database_path: String,
    pub executable: Option<String>,
    pub hostname: Option<String>,
    pub engine_version: String,
}

impl From<OwnerMetadata> for EngineOwnerMetadata {
    fn from(value: OwnerMetadata) -> Self {
        Self {
            protocol_version: value.protocol_version,
            owner_id: value.owner_id,
            owner_label: value.owner_label,
            process_id: value.process_id,
            started_at_unix_ms: value.started_at_unix_ms,
            database_path: value.database_path,
            executable: value.executable,
            hostname: value.hostname,
            engine_version: value.engine_version,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineObservationStatus {
    pub state: String,
    pub reconcile_in_flight: bool,
    pub dirty_instances: u32,
    pub full_reconcile_required: bool,
    pub recovery_required: bool,
    pub supervisors_running: u32,
    pub watched_instances: u32,
    pub watch_roots: u32,
    pub reconciles_total: f64,
    pub failed_reconciles_total: f64,
    pub retry_signals_total: f64,
    pub queue_overflows_total: f64,
    pub last_commit_seq: Option<f64>,
    pub last_started_at_unix_ms: Option<f64>,
    pub last_finished_at_unix_ms: Option<f64>,
    pub last_error: Option<String>,
}

impl From<ObservationStatusSnapshot> for EngineObservationStatus {
    fn from(value: ObservationStatusSnapshot) -> Self {
        Self {
            state: value.state,
            reconcile_in_flight: value.reconcile_in_flight,
            dirty_instances: value.dirty_instances,
            full_reconcile_required: value.full_reconcile_required,
            recovery_required: value.recovery_required,
            supervisors_running: value.supervisors_running,
            watched_instances: value.watched_instances,
            watch_roots: value.watch_roots,
            reconciles_total: value.reconciles_total as f64,
            failed_reconciles_total: value.failed_reconciles_total as f64,
            retry_signals_total: value.retry_signals_total as f64,
            queue_overflows_total: value.queue_overflows_total as f64,
            last_commit_seq: value.last_commit_seq.map(|number| number as f64),
            last_started_at_unix_ms: value.last_started_at_unix_ms.map(|number| number as f64),
            last_finished_at_unix_ms: value.last_finished_at_unix_ms.map(|number| number as f64),
            last_error: value.last_error,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub state: String,
    pub database_path: String,
    pub accepting_queries: bool,
    pub writer_alive: bool,
    pub configured_query_workers: u32,
    pub alive_query_workers: u32,
    pub in_flight_queries: u32,
    pub observation: EngineObservationStatus,
    pub owner: Option<EngineOwnerMetadata>,
}

impl From<EngineStatusSnapshot> for EngineStatus {
    fn from(value: EngineStatusSnapshot) -> Self {
        Self {
            state: value.state,
            database_path: value.database_path,
            accepting_queries: value.accepting_queries,
            writer_alive: value.writer_alive,
            configured_query_workers: value.configured_query_workers,
            alive_query_workers: value.alive_query_workers,
            in_flight_queries: value.in_flight_queries,
            observation: value.observation.into(),
            owner: value.owner.map(Into::into),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHealth {
    pub status: EngineStatus,
    pub healthy: bool,
    pub detail: Option<String>,
}

impl From<EngineHealthSnapshot> for EngineHealth {
    fn from(value: EngineHealthSnapshot) -> Self {
        Self {
            status: value.status.into(),
            healthy: value.healthy,
            detail: value.detail,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineOverviewResult {
    pub schema_version: u32,
    /// Latest durable ingest commit visible to the read-only query snapshot.
    pub commit_seq: f64,
    /// Transitional compatibility-table counts.
    pub projects: u32,
    pub sessions: u32,
    pub messages: u32,
    /// Canonical history materialized by RFC 011 observation commits.
    pub canonical_sessions: u32,
    pub canonical_messages: u32,
    /// Oldest durable change still resumable without taking a new snapshot.
    pub change_log_oldest_cursor: Option<EngineChangeCursor>,
    pub change_log_pruned_through_seq: f64,
    pub change_log_retained_changes: f64,
    pub change_log_retained_payload_bytes: f64,
    pub writer_data_version: u32,
    pub journal_mode: String,
    pub query_only: bool,
    pub read_only: bool,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineChangeCursor {
    pub commit_seq: f64,
    pub ordinal: u32,
}

impl From<ChangeCursor> for EngineChangeCursor {
    fn from(value: ChangeCursor) -> Self {
        Self {
            commit_seq: value.commit_seq as f64,
            ordinal: value.ordinal,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineChangeReplayOptions {
    /// Return changes strictly after this durable cursor.
    pub after: Option<EngineChangeCursor>,
    /// Empty or omitted means all stable topics.
    pub topics: Option<Vec<String>>,
    /// Page size. Defaults to 100 and is capped at 1,000 in Rust.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCommitWaitOptions {
    /// Resolve after the sole writer publishes a strictly newer commit.
    pub after_commit_seq: f64,
    /// Bounded recovery timeout. Defaults to 30 seconds; maximum 5 minutes.
    pub timeout_ms: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCommitWaitResult {
    pub observed_commit_seq: f64,
    /// `commit` or `timeout`.
    pub reason: String,
    pub waited_ms: f64,
}

impl From<CommitWaitResult> for EngineCommitWaitResult {
    fn from(value: CommitWaitResult) -> Self {
        Self {
            observed_commit_seq: value.observed_commit_seq as f64,
            reason: value.reason,
            waited_ms: value.waited_ms as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineDurableChange {
    pub cursor: EngineChangeCursor,
    pub topic: String,
    pub schema_version: u32,
    pub entity_key_base64url: String,
    pub operation: String,
    pub payload_base64: String,
}

impl From<DurableChange> for EngineDurableChange {
    fn from(value: DurableChange) -> Self {
        Self {
            cursor: value.cursor.into(),
            topic: value.topic,
            schema_version: value.schema_version,
            entity_key_base64url: URL_SAFE_NO_PAD.encode(value.entity_key),
            operation: value.operation,
            payload_base64: STANDARD.encode(value.payload),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineChangeReplay {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub oldest_available: Option<EngineChangeCursor>,
    pub changes: Vec<EngineDurableChange>,
    pub next_cursor: Option<EngineChangeCursor>,
    pub has_more: bool,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
}

impl From<ChangeReplay> for EngineChangeReplay {
    fn from(value: ChangeReplay) -> Self {
        debug_assert_eq!(value.contract_version, CHANGE_REPLAY_CONTRACT_VERSION);
        debug_assert_eq!(value.payload_byte_limit, MAX_CHANGE_REPLAY_PAYLOAD_BYTES);
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            oldest_available: value.oldest_available.map(Into::into),
            changes: value.changes.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor.map(Into::into),
            has_more: value.has_more,
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistoryPageOptions {
    /// Opaque keyset cursor returned by the preceding page.
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistorySessionPageOptions {
    /// Opaque project identity returned by `listHistoryProjects`.
    pub project_id: String,
    /// Opaque keyset cursor returned by the preceding page.
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistoryProjectIndex {
    pub status: String,
    pub original_path: Option<String>,
    pub entry_count: f64,
    pub assertion_count: f64,
    pub competing_snapshot_count: f64,
    pub last_commit_seq: f64,
}

impl From<HistoryProjectIndexSummary> for EngineHistoryProjectIndex {
    fn from(value: HistoryProjectIndexSummary) -> Self {
        Self {
            status: value.status,
            original_path: value.original_path,
            entry_count: value.entry_count as f64,
            assertion_count: value.assertion_count as f64,
            competing_snapshot_count: value.competing_snapshot_count as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistoryProject {
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_project_key: String,
    pub transcript_session_count: f64,
    pub message_count: f64,
    pub memory_document_count: f64,
    pub has_memory_index: bool,
    pub latest_activity_at: Option<String>,
    pub latest_activity_source: Option<String>,
    pub index: Option<EngineHistoryProjectIndex>,
    pub last_commit_seq: f64,
}

impl From<HistoryProjectSummary> for EngineHistoryProject {
    fn from(value: HistoryProjectSummary) -> Self {
        Self {
            project_id: value.project_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_project_key: value.native_project_key,
            transcript_session_count: value.transcript_session_count as f64,
            message_count: value.message_count as f64,
            memory_document_count: value.memory_document_count as f64,
            has_memory_index: value.has_memory_index,
            latest_activity_at: value.latest_activity_at,
            latest_activity_source: value.latest_activity_source,
            index: value.index.map(Into::into),
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistoryProjectPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub items: Vec<EngineHistoryProject>,
    pub next_cursor: Option<String>,
}

impl From<HistoryProjectPage> for EngineHistoryProjectPage {
    fn from(value: HistoryProjectPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistorySessionIndex {
    pub full_path: String,
    pub file_mtime_ms: f64,
    pub first_prompt: String,
    pub summary: Option<String>,
    pub message_count: f64,
    pub created_at: String,
    pub created_at_quality: String,
    pub modified_at: String,
    pub modified_at_quality: String,
    pub git_branch: String,
    pub project_path: String,
    pub is_sidechain: bool,
    pub transcript_status: String,
    pub resolution_status: String,
    pub assertion_count: f64,
    pub competing_entry_count: f64,
    pub identity_conflict: bool,
    pub join_conflict: bool,
    pub last_commit_seq: f64,
}

impl From<HistorySessionIndexSummary> for EngineHistorySessionIndex {
    fn from(value: HistorySessionIndexSummary) -> Self {
        Self {
            full_path: value.full_path,
            file_mtime_ms: value.file_mtime_ms as f64,
            first_prompt: value.first_prompt,
            summary: value.summary,
            message_count: value.message_count as f64,
            created_at: value.created_at,
            created_at_quality: value.created_at_quality,
            modified_at: value.modified_at,
            modified_at_quality: value.modified_at_quality,
            git_branch: value.git_branch,
            project_path: value.project_path,
            is_sidechain: value.is_sidechain,
            transcript_status: value.transcript_status,
            resolution_status: value.resolution_status,
            assertion_count: value.assertion_count as f64,
            competing_entry_count: value.competing_entry_count as f64,
            identity_conflict: value.identity_conflict,
            join_conflict: value.join_conflict,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

impl From<SessionIndexDetail> for EngineHistorySessionIndex {
    fn from(value: SessionIndexDetail) -> Self {
        Self {
            full_path: value.full_path,
            file_mtime_ms: value.file_mtime_ms as f64,
            first_prompt: value.first_prompt,
            summary: value.summary,
            message_count: value.message_count as f64,
            created_at: value.created_at,
            created_at_quality: value.created_at_quality,
            modified_at: value.modified_at,
            modified_at_quality: value.modified_at_quality,
            git_branch: value.git_branch,
            project_path: value.project_path,
            is_sidechain: value.is_sidechain,
            transcript_status: value.transcript_status,
            resolution_status: value.resolution_status,
            assertion_count: value.assertion_count as f64,
            competing_entry_count: value.competing_entry_count as f64,
            identity_conflict: value.identity_conflict,
            join_conflict: value.join_conflict,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistorySession {
    pub session_id: String,
    pub project_id: String,
    pub native_session_id: String,
    pub native_project_key: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
    pub message_count: f64,
    pub first_message_at: Option<String>,
    pub first_message_time_quality: Option<String>,
    pub last_message_at: Option<String>,
    pub last_message_time_quality: Option<String>,
    pub latest_activity_at: Option<String>,
    pub latest_activity_source: Option<String>,
    pub index: Option<EngineHistorySessionIndex>,
    pub last_commit_seq: f64,
}

impl From<HistorySessionSummary> for EngineHistorySession {
    fn from(value: HistorySessionSummary) -> Self {
        Self {
            session_id: value.session_id,
            project_id: value.project_id,
            native_session_id: value.native_session_id,
            native_project_key: value.native_project_key,
            cwd: value.cwd,
            git_branch: value.git_branch,
            first_prompt: value.first_prompt,
            ai_title: value.ai_title,
            custom_title: value.custom_title,
            message_count: value.message_count as f64,
            first_message_at: value.first_message_at,
            first_message_time_quality: value.first_message_time_quality,
            last_message_at: value.last_message_at,
            last_message_time_quality: value.last_message_time_quality,
            latest_activity_at: value.latest_activity_at,
            latest_activity_source: value.latest_activity_source,
            index: value.index.map(Into::into),
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineHistorySessionPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub items: Vec<EngineHistorySession>,
    pub next_cursor: Option<String>,
}

impl From<HistorySessionPage> for EngineHistorySessionPage {
    fn from(value: HistorySessionPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineMessagePageOptions {
    /// Opaque project identity returned by `listHistoryProjects`.
    pub project_id: String,
    /// Opaque session identity returned by `listHistorySessions`.
    pub session_id: String,
    /// Opaque keyset cursor returned by the preceding page.
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSessionDetail {
    pub session_id: String,
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_session_id: String,
    pub native_project_key: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub message_count: f64,
    pub run_count: f64,
    pub presence_count: f64,
    pub task_collection_count: f64,
    pub artifact_count: f64,
    pub workflow_count: f64,
    pub persisted_tool_result_count: f64,
    pub project_memory_document_count: f64,
    pub index: Option<EngineHistorySessionIndex>,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<SessionDetail> for EngineSessionDetail {
    fn from(value: SessionDetail) -> Self {
        Self {
            session_id: value.session_id,
            project_id: value.project_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_session_id: value.native_session_id,
            native_project_key: value.native_project_key,
            cwd: value.cwd,
            git_branch: value.git_branch,
            first_prompt: value.first_prompt,
            ai_title: value.ai_title,
            custom_title: value.custom_title,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            message_count: value.message_count as f64,
            run_count: value.run_count as f64,
            presence_count: value.presence_count as f64,
            task_collection_count: value.task_collection_count as f64,
            artifact_count: value.artifact_count as f64,
            workflow_count: value.workflow_count as f64,
            persisted_tool_result_count: value.persisted_tool_result_count as f64,
            project_memory_document_count: value.project_memory_document_count as f64,
            index: value.index.map(Into::into),
            decisive_fact_id: value.decisive_fact_id,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSessionDetails {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub session: Option<EngineSessionDetail>,
}

impl From<SessionDetails> for EngineSessionDetails {
    fn from(value: SessionDetails) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            session: value.session.map(Into::into),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineMessageDetail {
    pub message_id: String,
    pub session_id: String,
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_session_id: String,
    pub native_project_key: String,
    pub native_message_id: Option<String>,
    pub native_kind: String,
    pub role: String,
    pub content: serde_json::Value,
    pub native_payload: serde_json::Value,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub parent_native_message_id: Option<String>,
    pub model: Option<String>,
    pub search_text: Option<String>,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<MessageDetail> for EngineMessageDetail {
    fn from(value: MessageDetail) -> Self {
        Self {
            message_id: value.message_id,
            session_id: value.session_id,
            project_id: value.project_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_session_id: value.native_session_id,
            native_project_key: value.native_project_key,
            native_message_id: value.native_message_id,
            native_kind: value.native_kind,
            role: value.role,
            content: value.content,
            native_payload: value.native_payload,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            parent_native_message_id: value.parent_native_message_id,
            model: value.model,
            search_text: value.search_text,
            decisive_fact_id: value.decisive_fact_id,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineMessagePage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<EngineMessageDetail>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<MessagePage> for EngineMessagePage {
    fn from(value: MessagePage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSearchPageOptions {
    /// Search text interpreted as one literal FTS phrase.
    pub text: String,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub adapter_ids: Option<Vec<String>>,
    pub roles: Option<Vec<String>>,
    pub native_kinds: Option<Vec<String>>,
    /// `all` (default), `root`, `delegated`, or `unknown`.
    pub branch_kind: Option<String>,
    /// Opaque rank/keyset cursor returned by the preceding page.
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSearchHit {
    pub message_id: String,
    pub project_id: Option<String>,
    pub session_id: String,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub branch_kind: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_project_key: Option<String>,
    pub native_session_id: Option<String>,
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
    pub snippet: String,
    /// SQLite FTS5 BM25 rank. Lower values sort first.
    pub score: f64,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<SearchHit> for EngineSearchHit {
    fn from(value: SearchHit) -> Self {
        Self {
            message_id: value.message_id,
            project_id: value.project_id,
            session_id: value.session_id,
            run_id: value.run_id,
            parent_run_id: value.parent_run_id,
            branch_kind: value.branch_kind,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_project_key: value.native_project_key,
            native_session_id: value.native_session_id,
            native_run_id: value.native_run_id,
            native_child_id: value.native_child_id,
            native_task_id: value.native_task_id,
            delegation_status: value.delegation_status,
            native_message_id: value.native_message_id,
            native_kind: value.native_kind,
            role: value.role,
            model: value.model,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            snippet: value.snippet,
            score: value.score,
            decisive_fact_id: value.decisive_fact_id,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSearchPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub query_syntax: String,
    pub score_direction: String,
    pub total_is_exact: bool,
    pub total: f64,
    pub items: Vec<EngineSearchHit>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<SearchPage> for EngineSearchPage {
    fn from(value: SearchPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            query_syntax: value.query_syntax,
            score_direction: value.score_direction,
            total_is_exact: value.total_is_exact,
            total: value.total as f64,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTimelinePageOptions {
    pub project_id: String,
    pub session_id: String,
    pub roles: Option<Vec<String>>,
    pub native_kinds: Option<Vec<String>>,
    pub include_content_kinds: Option<Vec<String>>,
    pub include_tool_names: Option<Vec<String>>,
    pub exclude_content_kinds: Option<Vec<String>>,
    pub exclude_tool_names: Option<Vec<String>>,
    /// Optional literal FTS phrase. Blank strings disable search filtering.
    pub search: Option<String>,
    /// `all` (default), `root`, `delegated`, or `unknown`.
    pub branch_kind: Option<String>,
    /// Opaque newest-first message keyset cursor.
    pub cursor: Option<String>,
    /// Page size. Defaults to 30 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTimelineFacets {
    pub total_messages: f64,
    pub roles: Vec<EngineNamedCount>,
    pub native_kinds: Vec<EngineNamedCount>,
    pub content_kinds: Vec<EngineNamedCount>,
    pub tool_names: Vec<EngineNamedCount>,
    pub branch_kinds: Vec<EngineNamedCount>,
}

impl From<TimelineFacets> for EngineTimelineFacets {
    fn from(value: TimelineFacets) -> Self {
        Self {
            total_messages: value.total_messages as f64,
            roles: value.roles.into_iter().map(Into::into).collect(),
            native_kinds: value.native_kinds.into_iter().map(Into::into).collect(),
            content_kinds: value.content_kinds.into_iter().map(Into::into).collect(),
            tool_names: value.tool_names.into_iter().map(Into::into).collect(),
            branch_kinds: value.branch_kinds.into_iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTimelineMessage {
    pub message_id: String,
    pub project_id: String,
    pub session_id: String,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub branch_kind: String,
    pub branch_anchor_message_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: f64,
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
    pub content: serde_json::Value,
    pub content_kinds: Vec<String>,
    pub tool_names: Vec<String>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub parent_native_message_id: Option<String>,
    pub model: Option<String>,
    pub decisive_fact_id: String,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<TimelineMessage> for EngineTimelineMessage {
    fn from(value: TimelineMessage) -> Self {
        Self {
            message_id: value.message_id,
            project_id: value.project_id,
            session_id: value.session_id,
            run_id: value.run_id,
            parent_run_id: value.parent_run_id,
            branch_kind: value.branch_kind,
            branch_anchor_message_id: value.branch_anchor_message_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_project_key: value.native_project_key,
            native_session_id: value.native_session_id,
            native_run_id: value.native_run_id,
            native_child_id: value.native_child_id,
            native_task_id: value.native_task_id,
            delegation_kind: value.delegation_kind,
            delegation_strength: value.delegation_strength,
            delegation_status: value.delegation_status,
            branch_tool_name: value.branch_tool_name,
            branch_label: value.branch_label,
            requested_agent_type: value.requested_agent_type,
            native_message_id: value.native_message_id,
            native_kind: value.native_kind,
            role: value.role,
            content: value.content,
            content_kinds: value.content_kinds,
            tool_names: value.tool_names,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            parent_native_message_id: value.parent_native_message_id,
            model: value.model,
            decisive_fact_id: value.decisive_fact_id,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTimelinePage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub session_id: String,
    pub order: String,
    pub search_syntax: String,
    pub total_is_exact: bool,
    pub total: f64,
    pub facets: EngineTimelineFacets,
    pub items: Vec<EngineTimelineMessage>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<TimelinePage> for EngineTimelinePage {
    fn from(value: TimelinePage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            order: value.order,
            search_syntax: value.search_syntax,
            total_is_exact: value.total_is_exact,
            total: value.total as f64,
            facets: value.facets.into(),
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineDelegationPageOptions {
    pub project_id: String,
    pub session_id: String,
    pub workflow_id: Option<String>,
    pub standalone_only: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineDelegationSummary {
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub project_id: String,
    pub session_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
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
    pub message_count: f64,
    pub workflow_member_count: f64,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub decisive_relation_fact_id: Option<String>,
    pub decisive_spawn_fact_id: Option<String>,
    pub decisive_metadata_fact_id: Option<String>,
    pub assertion_count: f64,
    pub competing_relation_count: f64,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<DelegationSummary> for EngineDelegationSummary {
    fn from(value: DelegationSummary) -> Self {
        Self {
            run_id: value.run_id,
            parent_run_id: value.parent_run_id,
            project_id: value.project_id,
            session_id: value.session_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_run_id: value.native_run_id,
            native_child_id: value.native_child_id,
            native_task_id: value.native_task_id,
            agent_type: value.agent_type,
            description: value.description,
            native_name: value.native_name,
            spawn_depth: value.spawn_depth,
            label: value.label,
            prompt: value.prompt,
            cwd: value.cwd,
            worktree_path: value.worktree_path,
            relation_kind: value.relation_kind,
            relation_strength: value.relation_strength,
            relation_status: value.relation_status,
            metadata_status: value.metadata_status,
            spawn_status: value.spawn_status,
            branch_tool_name: value.branch_tool_name,
            requested_agent_type: value.requested_agent_type,
            branch_anchor_message_id: value.branch_anchor_message_id,
            child_present: value.child_present,
            parent_present: value.parent_present,
            metadata_run_present: value.metadata_run_present,
            observed_run_state: value.observed_run_state,
            message_count: value.message_count as f64,
            workflow_member_count: value.workflow_member_count as f64,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            decisive_relation_fact_id: value.decisive_relation_fact_id,
            decisive_spawn_fact_id: value.decisive_spawn_fact_id,
            decisive_metadata_fact_id: value.decisive_metadata_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_relation_count: value.competing_relation_count as f64,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineDelegationPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub session_id: String,
    pub workflow_id: Option<String>,
    pub standalone_only: bool,
    pub items: Vec<EngineDelegationSummary>,
    pub next_cursor: Option<String>,
}

impl From<DelegationPage> for EngineDelegationPage {
    fn from(value: DelegationPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            workflow_id: value.workflow_id,
            standalone_only: value.standalone_only,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWorkflowPageOptions {
    pub project_id: String,
    pub session_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWorkflowSummary {
    pub workflow_id: String,
    pub project_id: String,
    pub session_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_workflow_id: String,
    pub native_task_id: Option<String>,
    pub name: Option<String>,
    pub native_status: Option<String>,
    pub workflow_status: Option<String>,
    pub started_at: Option<String>,
    pub started_at_quality: Option<String>,
    pub finished_at: Option<String>,
    pub finished_at_quality: Option<String>,
    pub duration_ms: Option<f64>,
    pub agent_count: Option<f64>,
    pub total_tokens: Option<f64>,
    pub total_tool_calls: Option<f64>,
    pub snapshot_status: String,
    pub resolution_status: String,
    pub decisive_snapshot_fact_id: Option<String>,
    pub provenance_fact_id: String,
    pub snapshot_assertion_count: f64,
    pub competing_snapshot_count: f64,
    pub observed_member_count: f64,
    pub started_member_count: f64,
    pub result_member_count: f64,
    pub unresolved_member_count: f64,
    pub conflicting_member_count: f64,
    pub membership_count_status: String,
    pub join_conflict: bool,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<WorkflowSummary> for EngineWorkflowSummary {
    fn from(value: WorkflowSummary) -> Self {
        Self {
            workflow_id: value.workflow_id,
            project_id: value.project_id,
            session_id: value.session_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_workflow_id: value.native_workflow_id,
            native_task_id: value.native_task_id,
            name: value.name,
            native_status: value.native_status,
            workflow_status: value.workflow_status,
            started_at: value.started_at,
            started_at_quality: value.started_at_quality,
            finished_at: value.finished_at,
            finished_at_quality: value.finished_at_quality,
            duration_ms: value.duration_ms.map(|value| value as f64),
            agent_count: value.agent_count.map(|value| value as f64),
            total_tokens: value.total_tokens.map(|value| value as f64),
            total_tool_calls: value.total_tool_calls.map(|value| value as f64),
            snapshot_status: value.snapshot_status,
            resolution_status: value.resolution_status,
            decisive_snapshot_fact_id: value.decisive_snapshot_fact_id,
            provenance_fact_id: value.provenance_fact_id,
            snapshot_assertion_count: value.snapshot_assertion_count as f64,
            competing_snapshot_count: value.competing_snapshot_count as f64,
            observed_member_count: value.observed_member_count as f64,
            started_member_count: value.started_member_count as f64,
            result_member_count: value.result_member_count as f64,
            unresolved_member_count: value.unresolved_member_count as f64,
            conflicting_member_count: value.conflicting_member_count as f64,
            membership_count_status: value.membership_count_status,
            join_conflict: value.join_conflict,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWorkflowPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<EngineWorkflowSummary>,
    pub next_cursor: Option<String>,
}

impl From<WorkflowPage> for EngineWorkflowPage {
    fn from(value: WorkflowPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWorkflowDetails {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub workflow: EngineWorkflowSummary,
    pub default_model: Option<String>,
    pub script: Option<String>,
    pub script_path: Option<String>,
    pub args: Option<String>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub native_snapshot: Option<serde_json::Value>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
}

impl From<WorkflowDetails> for EngineWorkflowDetails {
    fn from(value: WorkflowDetails) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            workflow: value.workflow.into(),
            default_model: value.default_model,
            script: value.script,
            script_path: value.script_path,
            args: value.args,
            summary: value.summary,
            error: value.error,
            native_snapshot: value.native_snapshot,
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWorkflowMemberPageOptions {
    pub workflow_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWorkflowMember {
    pub member_id: String,
    pub workflow_id: String,
    pub project_id: String,
    pub session_id: String,
    pub child_run_id: String,
    pub child_run_present: bool,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_workflow_id: String,
    pub native_agent_id: String,
    pub native_event_key: String,
    pub native_run_id: Option<String>,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub native_name: Option<String>,
    pub worktree_path: Option<String>,
    pub member_status: String,
    pub result: Option<serde_json::Value>,
    pub resolution_status: String,
    pub observed_run_state: Option<String>,
    pub delegation_status: Option<String>,
    pub message_count: f64,
    pub decisive_started_fact_id: Option<String>,
    pub decisive_result_fact_id: Option<String>,
    pub started_observed_at_unix_ms: Option<f64>,
    pub result_observed_at_unix_ms: Option<f64>,
    pub started_assertion_count: f64,
    pub competing_started_count: f64,
    pub result_assertion_count: f64,
    pub competing_result_count: f64,
    pub event_key_conflict: bool,
    pub identity_conflict: bool,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<WorkflowMember> for EngineWorkflowMember {
    fn from(value: WorkflowMember) -> Self {
        Self {
            member_id: value.member_id,
            workflow_id: value.workflow_id,
            project_id: value.project_id,
            session_id: value.session_id,
            child_run_id: value.child_run_id,
            child_run_present: value.child_run_present,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_workflow_id: value.native_workflow_id,
            native_agent_id: value.native_agent_id,
            native_event_key: value.native_event_key,
            native_run_id: value.native_run_id,
            agent_type: value.agent_type,
            description: value.description,
            native_name: value.native_name,
            worktree_path: value.worktree_path,
            member_status: value.member_status,
            result: value.result,
            resolution_status: value.resolution_status,
            observed_run_state: value.observed_run_state,
            delegation_status: value.delegation_status,
            message_count: value.message_count as f64,
            decisive_started_fact_id: value.decisive_started_fact_id,
            decisive_result_fact_id: value.decisive_result_fact_id,
            started_observed_at_unix_ms: value
                .started_observed_at_unix_ms
                .map(|value| value as f64),
            result_observed_at_unix_ms: value.result_observed_at_unix_ms.map(|value| value as f64),
            started_assertion_count: value.started_assertion_count as f64,
            competing_started_count: value.competing_started_count as f64,
            result_assertion_count: value.result_assertion_count as f64,
            competing_result_count: value.competing_result_count as f64,
            event_key_conflict: value.event_key_conflict,
            identity_conflict: value.identity_conflict,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWorkflowMemberPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub workflow_id: String,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<EngineWorkflowMember>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<WorkflowMemberPage> for EngineWorkflowMemberPage {
    fn from(value: WorkflowMemberPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            workflow_id: value.workflow_id,
            project_id: value.project_id,
            session_id: value.session_id,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCapabilityPageOptions {
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineMemoryDocumentPageOptions {
    pub project_id: String,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineMemoryDocument {
    pub document_id: String,
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_project_key: String,
    pub native_document_path: String,
    pub title: String,
    pub content: String,
    pub size_bytes: f64,
    pub is_index: bool,
    pub resolution_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_document_count: f64,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<MemoryDocument> for EngineMemoryDocument {
    fn from(value: MemoryDocument) -> Self {
        Self {
            document_id: value.document_id,
            project_id: value.project_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_project_key: value.native_project_key,
            native_document_path: value.native_document_path,
            title: value.title,
            content: value.content,
            size_bytes: value.size_bytes as f64,
            is_index: value.is_index,
            resolution_status: value.resolution_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_document_count: value.competing_document_count as f64,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineMemoryDocumentPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub items: Vec<EngineMemoryDocument>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<MemoryDocumentPage> for EngineMemoryDocumentPage {
    fn from(value: MemoryDocumentPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTaskCollectionPageOptions {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub team_id: Option<String>,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTaskCollection {
    pub collection_id: String,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub team_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_collection_id: String,
    pub native_owner_id: Option<String>,
    pub collection_kind: String,
    pub native_collection_kind: String,
    pub resolution_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_metadata_count: f64,
    pub complete_snapshot_count: f64,
    pub item_document_count: f64,
    pub item_count: f64,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<TaskCollectionSummary> for EngineTaskCollection {
    fn from(value: TaskCollectionSummary) -> Self {
        Self {
            collection_id: value.collection_id,
            project_id: value.project_id,
            session_id: value.session_id,
            run_id: value.run_id,
            team_id: value.team_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_collection_id: value.native_collection_id,
            native_owner_id: value.native_owner_id,
            collection_kind: value.collection_kind,
            native_collection_kind: value.native_collection_kind,
            resolution_status: value.resolution_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_metadata_count: value.competing_metadata_count as f64,
            complete_snapshot_count: value.complete_snapshot_count as f64,
            item_document_count: value.item_document_count as f64,
            item_count: value.item_count as f64,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTaskCollectionPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub team_id: Option<String>,
    pub items: Vec<EngineTaskCollection>,
    pub next_cursor: Option<String>,
}

impl From<TaskCollectionPage> for EngineTaskCollectionPage {
    fn from(value: TaskCollectionPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            session_id: value.session_id,
            run_id: value.run_id,
            team_id: value.team_id,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTaskPageOptions {
    pub collection_id: String,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTask {
    pub task_id: String,
    pub collection_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub item_ordinal: f64,
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
    pub assertion_count: f64,
    pub competing_item_count: f64,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<TaskDetail> for EngineTask {
    fn from(value: TaskDetail) -> Self {
        Self {
            task_id: value.task_id,
            collection_id: value.collection_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            item_ordinal: value.item_ordinal as f64,
            native_task_id: value.native_task_id,
            subject: value.subject,
            description: value.description,
            active_form: value.active_form,
            native_owner: value.native_owner,
            task_status: value.task_status,
            native_status: value.native_status,
            blocks: value.blocks,
            blocked_by: value.blocked_by,
            resolution_status: value.resolution_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_item_count: value.competing_item_count as f64,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTaskPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub collection_id: String,
    pub items: Vec<EngineTask>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<TaskPage> for EngineTaskPage {
    fn from(value: TaskPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            collection_id: value.collection_id,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EnginePlan {
    pub plan_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_plan_id: String,
    pub title: String,
    pub content: String,
    pub size_bytes: f64,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub resolution_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_plan_count: f64,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<PlanDetail> for EnginePlan {
    fn from(value: PlanDetail) -> Self {
        Self {
            plan_id: value.plan_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_plan_id: value.native_plan_id,
            title: value.title,
            content: value.content,
            size_bytes: value.size_bytes as f64,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            resolution_status: value.resolution_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_plan_count: value.competing_plan_count as f64,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EnginePlanPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub items: Vec<EnginePlan>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<PlanPage> for EnginePlanPage {
    fn from(value: PlanPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineToolResultPageOptions {
    pub project_id: String,
    pub session_id: String,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineToolResult {
    pub result_id: String,
    pub project_id: String,
    pub session_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_project_key: String,
    pub native_session_id: String,
    pub native_tool_use_id: String,
    pub native_document_path: String,
    pub content: String,
    pub size_bytes: f64,
    pub resolution_status: String,
    pub correlation_status: String,
    pub tool_call_message_id: Option<String>,
    pub tool_result_message_id: Option<String>,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_result_count: f64,
    pub tool_call_match_count: f64,
    pub tool_result_match_count: f64,
    pub join_conflict: bool,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<ToolResultDetail> for EngineToolResult {
    fn from(value: ToolResultDetail) -> Self {
        Self {
            result_id: value.result_id,
            project_id: value.project_id,
            session_id: value.session_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_project_key: value.native_project_key,
            native_session_id: value.native_session_id,
            native_tool_use_id: value.native_tool_use_id,
            native_document_path: value.native_document_path,
            content: value.content,
            size_bytes: value.size_bytes as f64,
            resolution_status: value.resolution_status,
            correlation_status: value.correlation_status,
            tool_call_message_id: value.tool_call_message_id,
            tool_result_message_id: value.tool_result_message_id,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_result_count: value.competing_result_count as f64,
            tool_call_match_count: value.tool_call_match_count as f64,
            tool_result_match_count: value.tool_result_match_count as f64,
            join_conflict: value.join_conflict,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineToolResultPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub session_id: String,
    pub items: Vec<EngineToolResult>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<ToolResultPage> for EngineToolResultPage {
    fn from(value: ToolResultPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineArtifactPageOptions {
    pub session_id: String,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineArtifact {
    pub artifact_id: String,
    pub session_id: String,
    pub project_id: Option<String>,
    pub native_artifact_id: Option<String>,
    pub native_file_hash: Option<String>,
    pub version: f64,
    pub tracking_path: Option<String>,
    pub real_parent_dir: Option<String>,
    pub backup_time: Option<String>,
    pub backup_time_quality: Option<String>,
    pub capture_status: String,
    pub content_base64: Option<String>,
    pub size_bytes: Option<f64>,
    pub content_digest_base64url: Option<String>,
    pub content_status: String,
    pub resolution_status: String,
    pub metadata_fact_id: Option<String>,
    pub content_fact_id: Option<String>,
    pub metadata_adapter_id: Option<String>,
    pub metadata_source_instance_id: Option<f64>,
    pub metadata_observed_at_unix_ms: Option<f64>,
    pub metadata_source_object_id: Option<f64>,
    pub metadata_source_generation: Option<f64>,
    pub content_adapter_id: Option<String>,
    pub content_source_instance_id: Option<f64>,
    pub content_observed_at_unix_ms: Option<f64>,
    pub content_source_object_id: Option<f64>,
    pub content_source_generation: Option<f64>,
    pub metadata_assertion_count: f64,
    pub competing_metadata_count: f64,
    pub content_assertion_count: f64,
    pub competing_content_count: f64,
    pub join_conflict: bool,
    pub last_commit_seq: f64,
}

impl From<ArtifactDetail> for EngineArtifact {
    fn from(value: ArtifactDetail) -> Self {
        Self {
            artifact_id: value.artifact_id,
            session_id: value.session_id,
            project_id: value.project_id,
            native_artifact_id: value.native_artifact_id,
            native_file_hash: value.native_file_hash,
            version: value.version as f64,
            tracking_path: value.tracking_path,
            real_parent_dir: value.real_parent_dir,
            backup_time: value.backup_time,
            backup_time_quality: value.backup_time_quality,
            capture_status: value.capture_status,
            content_base64: value.content_base64,
            size_bytes: value.size_bytes.map(|number| number as f64),
            content_digest_base64url: value.content_digest_base64url,
            content_status: value.content_status,
            resolution_status: value.resolution_status,
            metadata_fact_id: value.metadata_fact_id,
            content_fact_id: value.content_fact_id,
            metadata_adapter_id: value.metadata_adapter_id,
            metadata_source_instance_id: value
                .metadata_source_instance_id
                .map(|number| number as f64),
            metadata_observed_at_unix_ms: value
                .metadata_observed_at_unix_ms
                .map(|number| number as f64),
            metadata_source_object_id: value.metadata_source_object_id.map(|number| number as f64),
            metadata_source_generation: value
                .metadata_source_generation
                .map(|number| number as f64),
            content_adapter_id: value.content_adapter_id,
            content_source_instance_id: value
                .content_source_instance_id
                .map(|number| number as f64),
            content_observed_at_unix_ms: value
                .content_observed_at_unix_ms
                .map(|number| number as f64),
            content_source_object_id: value.content_source_object_id.map(|number| number as f64),
            content_source_generation: value.content_source_generation.map(|number| number as f64),
            metadata_assertion_count: value.metadata_assertion_count as f64,
            competing_metadata_count: value.competing_metadata_count as f64,
            content_assertion_count: value.content_assertion_count as f64,
            competing_content_count: value.competing_content_count as f64,
            join_conflict: value.join_conflict,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineArtifactPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub session_id: String,
    pub items: Vec<EngineArtifact>,
    pub payload_bytes: f64,
    pub payload_byte_limit: f64,
    pub next_cursor: Option<String>,
}

impl From<ArtifactPage> for EngineArtifactPage {
    fn from(value: ArtifactPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            session_id: value.session_id,
            items: value.items.into_iter().map(Into::into).collect(),
            payload_bytes: value.payload_bytes as f64,
            payload_byte_limit: value.payload_byte_limit as f64,
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSourceCapability {
    pub id: String,
    pub support_level: String,
    pub granularity: String,
    pub availability: String,
    pub notes: Option<String>,
}

impl From<SourceCapabilitySummary> for EngineSourceCapability {
    fn from(value: SourceCapabilitySummary) -> Self {
        Self {
            id: value.id,
            support_level: value.support_level,
            granularity: value.granularity,
            availability: value.availability,
            notes: value.notes,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSourceSummary {
    pub source_id: String,
    pub source_instance_id: f64,
    pub adapter_id: String,
    pub display_name: String,
    pub adapter_version: String,
    pub adapter_contract_version: u32,
    pub source_schema_versions: Vec<String>,
    pub capabilities: Vec<EngineSourceCapability>,
    pub discovered_at_unix_ms: f64,
    pub last_seen_at_unix_ms: f64,
    pub stream_count: f64,
    pub unavailable_stream_count: f64,
    pub object_count: f64,
    pub active_object_count: f64,
    pub record_error_count: f64,
    pub fact_count: f64,
    pub commit_count: f64,
    pub last_commit_seq: Option<f64>,
}

impl From<SourceSummary> for EngineSourceSummary {
    fn from(value: SourceSummary) -> Self {
        Self {
            source_id: value.source_id,
            source_instance_id: value.source_instance_id as f64,
            adapter_id: value.adapter_id,
            display_name: value.display_name,
            adapter_version: value.adapter_version,
            adapter_contract_version: value.adapter_contract_version,
            source_schema_versions: value.source_schema_versions,
            capabilities: value.capabilities.into_iter().map(Into::into).collect(),
            discovered_at_unix_ms: value.discovered_at_unix_ms as f64,
            last_seen_at_unix_ms: value.last_seen_at_unix_ms as f64,
            stream_count: value.stream_count as f64,
            unavailable_stream_count: value.unavailable_stream_count as f64,
            object_count: value.object_count as f64,
            active_object_count: value.active_object_count as f64,
            record_error_count: value.record_error_count as f64,
            fact_count: value.fact_count as f64,
            commit_count: value.commit_count as f64,
            last_commit_seq: value.last_commit_seq.map(|number| number as f64),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSourcePage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub items: Vec<EngineSourceSummary>,
    pub next_cursor: Option<String>,
}

impl From<SourcePage> for EngineSourcePage {
    fn from(value: SourcePage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineNamedCount {
    pub name: String,
    pub count: f64,
}

impl From<NamedCount> for EngineNamedCount {
    fn from(value: NamedCount) -> Self {
        Self {
            name: value.name,
            count: value.count as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineLatencyStats {
    pub samples: f64,
    pub total_ms: f64,
    pub mean_ms: f64,
    pub max_ms: f64,
    pub p50_upper_ms: f64,
    pub p95_upper_ms: f64,
    pub p99_upper_ms: f64,
}

impl From<crate::engine::LatencySnapshot> for EngineLatencyStats {
    fn from(value: crate::engine::LatencySnapshot) -> Self {
        let total_ms = ns_to_ms(value.total_ns);
        Self {
            samples: value.samples as f64,
            total_ms,
            mean_ms: if value.samples == 0 {
                0.0
            } else {
                total_ms / value.samples as f64
            },
            max_ms: ns_to_ms(value.max_ns),
            p50_upper_ms: ns_to_ms(value.p50_upper_ns),
            p95_upper_ms: ns_to_ms(value.p95_upper_ns),
            p99_upper_ms: ns_to_ms(value.p99_upper_ns),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineNamedLatencyStats {
    pub name: String,
    pub latency: EngineLatencyStats,
}

impl From<NamedLatencySnapshot> for EngineNamedLatencyStats {
    fn from(value: NamedLatencySnapshot) -> Self {
        Self {
            name: value.name,
            latency: value.latency.into(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineWriterPerformanceStats {
    pub uptime_ms: f64,
    pub commit_attempts: f64,
    pub committed: f64,
    pub failed: f64,
    pub facts_committed: f64,
    pub changes_published: f64,
    pub sqlite_rows_changed: f64,
    pub queue_depth: f64,
    pub queue_high_watermark: f64,
    pub checkpoint: EngineCheckpointPerformanceStats,
    pub timings: Vec<EngineNamedLatencyStats>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCheckpointPerformanceStats {
    pub attempts: f64,
    pub completed: f64,
    pub blocked: f64,
    pub failures: f64,
    pub last_log_frames: f64,
    pub last_checkpointed_frames: f64,
    pub last_remaining_frames: f64,
    pub blocked_by_reader_ms: f64,
    pub latency: EngineLatencyStats,
}

impl From<CheckpointPerformanceSnapshot> for EngineCheckpointPerformanceStats {
    fn from(value: CheckpointPerformanceSnapshot) -> Self {
        Self {
            attempts: value.attempts as f64,
            completed: value.completed as f64,
            blocked: value.blocked as f64,
            failures: value.failures as f64,
            last_log_frames: value.last_log_frames as f64,
            last_checkpointed_frames: value.last_checkpointed_frames as f64,
            last_remaining_frames: value.last_remaining_frames as f64,
            blocked_by_reader_ms: ns_to_ms(value.blocked_by_reader_ns),
            latency: value.latency.into(),
        }
    }
}

impl From<WriterPerformanceSnapshot> for EngineWriterPerformanceStats {
    fn from(value: WriterPerformanceSnapshot) -> Self {
        Self {
            uptime_ms: ns_to_ms(value.uptime_ns),
            commit_attempts: value.commit_attempts as f64,
            committed: value.committed as f64,
            failed: value.failed as f64,
            facts_committed: value.facts_committed as f64,
            changes_published: value.changes_published as f64,
            sqlite_rows_changed: value.sqlite_rows_changed as f64,
            queue_depth: value.queue_depth as f64,
            queue_high_watermark: value.queue_high_watermark as f64,
            checkpoint: value.checkpoint.into(),
            timings: value.timings.into_iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineQueryPerformanceStats {
    pub uptime_ms: f64,
    pub requests_enqueued: f64,
    pub requests_completed: f64,
    pub queue_rejections: f64,
    pub queue_depth: f64,
    pub queue_high_watermark: f64,
    pub oldest_active_ms: f64,
    pub timings: Vec<EngineNamedLatencyStats>,
}

impl From<QueryPerformanceSnapshot> for EngineQueryPerformanceStats {
    fn from(value: QueryPerformanceSnapshot) -> Self {
        Self {
            uptime_ms: ns_to_ms(value.uptime_ns),
            requests_enqueued: value.requests_enqueued as f64,
            requests_completed: value.requests_completed as f64,
            queue_rejections: value.queue_rejections as f64,
            queue_depth: value.queue_depth as f64,
            queue_high_watermark: value.queue_high_watermark as f64,
            oldest_active_ms: ns_to_ms(value.oldest_active_ns),
            timings: value.timings.into_iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSourcePipelineStats {
    pub read_attempts: f64,
    pub read_failures: f64,
    pub read_retries: f64,
    pub read_continuations: f64,
    pub records_read: f64,
    pub payload_bytes_read: f64,
    pub decode_attempts: f64,
    pub decode_failures: f64,
    pub decode_retries: f64,
    pub records_decoded: f64,
    pub facts_emitted: f64,
    pub records_quarantined: f64,
    pub timings: Vec<EngineNamedLatencyStats>,
}

impl From<SourcePipelineSnapshot> for EngineSourcePipelineStats {
    fn from(value: SourcePipelineSnapshot) -> Self {
        Self {
            read_attempts: value.read_attempts as f64,
            read_failures: value.read_failures as f64,
            read_retries: value.read_retries as f64,
            read_continuations: value.read_continuations as f64,
            records_read: value.records_read as f64,
            payload_bytes_read: value.payload_bytes_read as f64,
            decode_attempts: value.decode_attempts as f64,
            decode_failures: value.decode_failures as f64,
            decode_retries: value.decode_retries as f64,
            records_decoded: value.records_decoded as f64,
            facts_emitted: value.facts_emitted as f64,
            records_quarantined: value.records_quarantined as f64,
            timings: value.timings.into_iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSourceDimensionPerformanceStats {
    pub adapter_id: String,
    pub stream_id: String,
    pub driver_kind: String,
    pub pipeline: EngineSourcePipelineStats,
}

impl From<SourceDimensionPerformanceSnapshot> for EngineSourceDimensionPerformanceStats {
    fn from(value: SourceDimensionPerformanceSnapshot) -> Self {
        Self {
            adapter_id: value.adapter_id,
            stream_id: value.stream_id,
            driver_kind: value.driver_kind,
            pipeline: value.pipeline.into(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineSourcePerformanceStats {
    pub uptime_ms: f64,
    pub dimension_capacity: f64,
    pub dimension_overflow_assignments: f64,
    pub totals: EngineSourcePipelineStats,
    pub dimensions: Vec<EngineSourceDimensionPerformanceStats>,
}

impl From<SourcePerformanceSnapshot> for EngineSourcePerformanceStats {
    fn from(value: SourcePerformanceSnapshot) -> Self {
        Self {
            uptime_ms: ns_to_ms(value.uptime_ns),
            dimension_capacity: value.dimension_capacity as f64,
            dimension_overflow_assignments: value.dimension_overflow_assignments as f64,
            totals: value.totals.into(),
            dimensions: value.dimensions.into_iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineStoragePerformanceStats {
    pub database_file_bytes: f64,
    pub wal_file_bytes: f64,
    pub shared_memory_file_bytes: f64,
}

impl From<StoragePerformanceSnapshot> for EngineStoragePerformanceStats {
    fn from(value: StoragePerformanceSnapshot) -> Self {
        Self {
            database_file_bytes: value.database_file_bytes as f64,
            wal_file_bytes: value.wal_file_bytes as f64,
            shared_memory_file_bytes: value.shared_memory_file_bytes as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EnginePerformanceStats {
    pub writer: EngineWriterPerformanceStats,
    pub queries: EngineQueryPerformanceStats,
    pub source: EngineSourcePerformanceStats,
    pub storage: EngineStoragePerformanceStats,
}

impl From<crate::engine::EnginePerformanceSnapshot> for EnginePerformanceStats {
    fn from(value: crate::engine::EnginePerformanceSnapshot) -> Self {
        Self {
            writer: value.writer.into(),
            queries: value.queries.into(),
            source: value.source.into(),
            storage: value.storage.into(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCanonicalStats {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub schema_version: u32,
    pub source_instances: f64,
    pub source_streams: f64,
    pub source_objects: f64,
    pub active_source_objects: f64,
    pub source_record_errors: f64,
    pub ingest_commits: f64,
    pub fact_records: f64,
    pub searchable_messages: f64,
    pub entities: Vec<EngineNamedCount>,
    pub source_stream_states: Vec<EngineNamedCount>,
    pub projection_readiness: Vec<EngineNamedCount>,
    pub database_page_count: f64,
    pub database_page_size_bytes: f64,
    pub allocated_database_bytes: f64,
    pub performance: Option<EnginePerformanceStats>,
}

impl From<CanonicalStats> for EngineCanonicalStats {
    fn from(value: CanonicalStats) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            schema_version: value.schema_version,
            source_instances: value.source_instances as f64,
            source_streams: value.source_streams as f64,
            source_objects: value.source_objects as f64,
            active_source_objects: value.active_source_objects as f64,
            source_record_errors: value.source_record_errors as f64,
            ingest_commits: value.ingest_commits as f64,
            fact_records: value.fact_records as f64,
            searchable_messages: value.searchable_messages as f64,
            entities: value.entities.into_iter().map(Into::into).collect(),
            source_stream_states: value
                .source_stream_states
                .into_iter()
                .map(Into::into)
                .collect(),
            projection_readiness: value
                .projection_readiness
                .into_iter()
                .map(Into::into)
                .collect(),
            database_page_count: value.database_page_count as f64,
            database_page_size_bytes: value.database_page_size_bytes as f64,
            allocated_database_bytes: value.allocated_database_bytes as f64,
            performance: value.performance.map(Into::into),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageScopeOptions {
    /// Opaque project identity returned by `listHistoryProjects`.
    pub project_id: String,
    /// Optional opaque session identity returned by `listHistorySessions`.
    pub session_id: Option<String>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageActivityOptions {
    /// Opaque project identity returned by `listHistoryProjects`.
    pub project_id: String,
    /// Optional opaque session identity returned by `listHistorySessions`.
    pub session_id: Option<String>,
    /// Inclusive calendar date in YYYY-MM-DD form.
    pub from: String,
    /// Inclusive calendar date in YYYY-MM-DD form.
    pub to: String,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageTokenValues {
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cache_creation_tokens: f64,
    pub cache_read_tokens: f64,
    /// Arithmetic sum of the four preserved native components. This is not a
    /// provider billing normalization.
    pub component_total_tokens: f64,
}

impl From<UsageTokenValues> for EngineUsageTokenValues {
    fn from(value: UsageTokenValues) -> Self {
        Self {
            input_tokens: value.input_tokens as f64,
            output_tokens: value.output_tokens as f64,
            cache_creation_tokens: value.cache_creation_tokens as f64,
            cache_read_tokens: value.cache_read_tokens as f64,
            component_total_tokens: value.component_total_tokens as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageAggregate {
    pub exact: EngineUsageTokenValues,
    pub estimated: EngineUsageTokenValues,
    pub combined: EngineUsageTokenValues,
    pub quality: String,
    pub exact_contribution_count: f64,
    pub estimated_contribution_count: f64,
    pub contribution_count: f64,
    pub session_count: f64,
}

impl From<UsageAggregate> for EngineUsageAggregate {
    fn from(value: UsageAggregate) -> Self {
        Self {
            exact: value.exact.into(),
            estimated: value.estimated.into(),
            combined: value.combined.into(),
            quality: value.quality,
            exact_contribution_count: value.exact_contribution_count as f64,
            estimated_contribution_count: value.estimated_contribution_count as f64,
            contribution_count: value.contribution_count as f64,
            session_count: value.session_count as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageCoverage {
    pub scope: String,
    pub accounting: String,
    pub value_quality: String,
    pub quality_bucket: String,
    pub model: Option<String>,
    pub source_time_quality: Option<String>,
    pub contribution_count: f64,
    pub tokens: EngineUsageTokenValues,
}

impl From<UsageCoverageSummary> for EngineUsageCoverage {
    fn from(value: UsageCoverageSummary) -> Self {
        Self {
            scope: value.scope,
            accounting: value.accounting,
            value_quality: value.value_quality,
            quality_bucket: value.quality_bucket,
            model: value.model,
            source_time_quality: value.source_time_quality,
            contribution_count: value.contribution_count as f64,
            tokens: value.tokens.into(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageTotals {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub session_id: Option<String>,
    pub aggregate: EngineUsageAggregate,
    pub coverage: Vec<EngineUsageCoverage>,
    pub first_source_time: Option<String>,
    pub last_source_time: Option<String>,
    pub first_observed_at_unix_ms: Option<f64>,
    pub last_observed_at_unix_ms: Option<f64>,
    pub last_commit_seq: Option<f64>,
}

impl From<UsageTotalsReport> for EngineUsageTotals {
    fn from(value: UsageTotalsReport) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            aggregate: value.aggregate.into(),
            coverage: value.coverage.into_iter().map(Into::into).collect(),
            first_source_time: value.first_source_time,
            last_source_time: value.last_source_time,
            first_observed_at_unix_ms: value.first_observed_at_unix_ms.map(|value| value as f64),
            last_observed_at_unix_ms: value.last_observed_at_unix_ms.map(|value| value as f64),
            last_commit_seq: value.last_commit_seq.map(|value| value as f64),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageActivityDay {
    pub date: String,
    pub aggregate: EngineUsageAggregate,
    pub first_source_time: String,
    pub last_source_time: String,
    pub first_observed_at_unix_ms: f64,
    pub last_observed_at_unix_ms: f64,
    pub last_commit_seq: f64,
}

impl From<UsageActivityDay> for EngineUsageActivityDay {
    fn from(value: UsageActivityDay) -> Self {
        Self {
            date: value.date,
            aggregate: value.aggregate.into(),
            first_source_time: value.first_source_time,
            last_source_time: value.last_source_time,
            first_observed_at_unix_ms: value.first_observed_at_unix_ms as f64,
            last_observed_at_unix_ms: value.last_observed_at_unix_ms as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUntimedUsage {
    pub aggregate: EngineUsageAggregate,
    pub coverage: Vec<EngineUsageCoverage>,
    pub first_observed_at_unix_ms: Option<f64>,
    pub last_observed_at_unix_ms: Option<f64>,
    pub last_commit_seq: Option<f64>,
}

impl From<UntimedUsageSummary> for EngineUntimedUsage {
    fn from(value: UntimedUsageSummary) -> Self {
        Self {
            aggregate: value.aggregate.into(),
            coverage: value.coverage.into_iter().map(Into::into).collect(),
            first_observed_at_unix_ms: value.first_observed_at_unix_ms.map(|value| value as f64),
            last_observed_at_unix_ms: value.last_observed_at_unix_ms.map(|value| value as f64),
            last_commit_seq: value.last_commit_seq.map(|value| value as f64),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageActivity {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: String,
    pub session_id: Option<String>,
    pub from: String,
    pub to: String,
    pub days: Vec<EngineUsageActivityDay>,
    pub aggregate: EngineUsageAggregate,
    pub coverage: Vec<EngineUsageCoverage>,
    pub untimed: EngineUntimedUsage,
    pub first_observed_at_unix_ms: Option<f64>,
    pub last_observed_at_unix_ms: Option<f64>,
    pub last_commit_seq: Option<f64>,
}

impl From<UsageActivityReport> for EngineUsageActivity {
    fn from(value: UsageActivityReport) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            from: value.from,
            to: value.to,
            days: value.days.into_iter().map(Into::into).collect(),
            aggregate: value.aggregate.into(),
            coverage: value.coverage.into_iter().map(Into::into).collect(),
            untimed: value.untimed.into(),
            first_observed_at_unix_ms: value.first_observed_at_unix_ms.map(|value| value as f64),
            last_observed_at_unix_ms: value.last_observed_at_unix_ms.map(|value| value as f64),
            last_commit_seq: value.last_commit_seq.map(|value| value as f64),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2Options {
    /// Opaque project identity returned by `listHistoryProjects`.
    pub project_id: String,
    /// Opaque session identity returned by `listHistorySessions`.
    pub session_id: String,
    /// Optional RFC 012A actor entity reference returned by this query.
    pub actor_run_ref: Option<String>,
    /// Optional `team` or `workflow` dimension; requires a target reference.
    pub affiliation_dimension: Option<String>,
    /// RFC 012A team/workflow target entity reference paired with dimension.
    pub affiliation_target_ref: Option<String>,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query pack.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineFactFamilyCoverageOptions {
    /// Opaque project identity returned by `listHistoryProjects`.
    pub project_id: String,
    /// Opaque session identity returned by `listHistorySessions`.
    pub session_id: String,
    /// Durable projection/coverage owner identifier.
    pub owner_id: String,
    /// Common fact-family identifier, for example `runtime.usage-v2`.
    pub family: String,
    pub family_version: u32,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query pack.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineFactFamilyCoverageSetSummary {
    pub coverage_set_contract_version: u32,
    pub coverage_contract_version: u32,
    pub adapter_id: String,
    pub source_instance_ref: String,
    pub support_release_id: String,
    pub declaration_ref: String,
    pub membership_revision_ref: String,
    pub completeness: String,
    pub content_digest_ref: String,
    pub last_commit_seq: f64,
    pub updated_at_unix_ms: f64,
}

impl From<FactFamilyCoverageSetSummary> for EngineFactFamilyCoverageSetSummary {
    fn from(value: FactFamilyCoverageSetSummary) -> Self {
        Self {
            coverage_set_contract_version: value.coverage_set_contract_version,
            coverage_contract_version: value.coverage_contract_version,
            adapter_id: value.adapter_id,
            source_instance_ref: value.source_instance_ref,
            support_release_id: value.support_release_id,
            declaration_ref: value.declaration_ref,
            membership_revision_ref: value.membership_revision_ref,
            completeness: value.completeness,
            content_digest_ref: value.content_digest_ref,
            last_commit_seq: value.last_commit_seq as f64,
            updated_at_unix_ms: value.updated_at_unix_ms as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineFactFamilyCoverageItem {
    pub kind: String,
    pub stream_ref: Option<String>,
    pub object_ref: Option<String>,
    pub generation: Option<f64>,
    pub position_kind: Option<String>,
    pub position_ref: Option<String>,
    pub monotonic_order: Option<f64>,
    pub status: Option<String>,
    pub unavailable_reason: Option<String>,
    pub source_record_ref: Option<String>,
    pub semantic_revision_ref: Option<String>,
    pub observed_at_unix_ms: Option<f64>,
    pub absence_kind: Option<String>,
    pub error_code: Option<String>,
}

impl From<FactFamilyCoverageItem> for EngineFactFamilyCoverageItem {
    fn from(value: FactFamilyCoverageItem) -> Self {
        Self {
            kind: value.kind,
            stream_ref: value.stream_ref,
            object_ref: value.object_ref,
            generation: value.generation.map(|value| value as f64),
            position_kind: value.position_kind,
            position_ref: value.position_ref,
            monotonic_order: value.monotonic_order.map(|value| value as f64),
            status: value.status,
            unavailable_reason: value.unavailable_reason,
            source_record_ref: value.source_record_ref,
            semantic_revision_ref: value.semantic_revision_ref,
            observed_at_unix_ms: value.observed_at_unix_ms.map(|value| value as f64),
            absence_kind: value.absence_kind,
            error_code: value.error_code,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineFactFamilyCoveragePage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub status: String,
    pub project_id: String,
    pub session_id: String,
    pub owner_id: String,
    pub family: String,
    pub family_version: u32,
    pub coverage: Option<EngineFactFamilyCoverageSetSummary>,
    pub items: Vec<EngineFactFamilyCoverageItem>,
    pub next_cursor: Option<String>,
}

impl From<FactFamilyCoveragePage> for EngineFactFamilyCoveragePage {
    fn from(value: FactFamilyCoveragePage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            status: value.status,
            project_id: value.project_id,
            session_id: value.session_id,
            owner_id: value.owner_id,
            family: value.family,
            family_version: value.family_version,
            coverage: value.coverage.map(Into::into),
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2ExternalEntityRef {
    pub external_entity_reference_version: u32,
    pub entity_key: String,
}

impl From<RuntimeUsageV2ExternalEntityRef> for EngineRuntimeUsageV2ExternalEntityRef {
    fn from(value: RuntimeUsageV2ExternalEntityRef) -> Self {
        Self {
            external_entity_reference_version: value.external_entity_reference_version,
            entity_key: value.entity_key,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2SemanticRevisionRef {
    pub semantic_reference_contract_version: u32,
    pub fact_revision_id: String,
}

impl From<RuntimeUsageV2SemanticRevisionRef> for EngineRuntimeUsageV2SemanticRevisionRef {
    fn from(value: RuntimeUsageV2SemanticRevisionRef) -> Self {
        Self {
            semantic_reference_contract_version: value.semantic_reference_contract_version,
            fact_revision_id: value.fact_revision_id,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2ValueProvenance {
    pub native_field: String,
    pub normalization_contract_version: u32,
}

impl From<RuntimeUsageV2ValueProvenance> for EngineRuntimeUsageV2ValueProvenance {
    fn from(value: RuntimeUsageV2ValueProvenance) -> Self {
        Self {
            native_field: value.native_field,
            normalization_contract_version: value.normalization_contract_version,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2TokenValue {
    pub value: Option<f64>,
    pub quality: String,
    pub authority: String,
    pub completeness: String,
    pub unknown_reason: Option<String>,
    pub effective_at: Option<f64>,
    pub provenance: EngineRuntimeUsageV2ValueProvenance,
}

impl From<RuntimeUsageV2TokenValue> for EngineRuntimeUsageV2TokenValue {
    fn from(value: RuntimeUsageV2TokenValue) -> Self {
        Self {
            value: value.value.map(|value| value as f64),
            quality: value.quality,
            authority: value.authority,
            completeness: value.completeness,
            unknown_reason: value.unknown_reason,
            effective_at: value.effective_at.map(|value| value as f64),
            provenance: value.provenance.into(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2TextValue {
    pub value: Option<String>,
    pub quality: String,
    pub authority: String,
    pub completeness: String,
    pub unknown_reason: Option<String>,
    pub effective_at: Option<f64>,
    pub provenance: EngineRuntimeUsageV2ValueProvenance,
}

impl From<RuntimeUsageV2TextValue> for EngineRuntimeUsageV2TextValue {
    fn from(value: RuntimeUsageV2TextValue) -> Self {
        Self {
            value: value.value,
            quality: value.quality,
            authority: value.authority,
            completeness: value.completeness,
            unknown_reason: value.unknown_reason,
            effective_at: value.effective_at.map(|value| value as f64),
            provenance: value.provenance.into(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2Response {
    pub usage_key: String,
    pub semantic_revision_ref: EngineRuntimeUsageV2SemanticRevisionRef,
    pub source_record_ref: String,
    pub session_ref: EngineRuntimeUsageV2ExternalEntityRef,
    pub actor_run_ref: EngineRuntimeUsageV2ExternalEntityRef,
    pub response_key_base64: String,
    pub response_identity: String,
    pub native_message_id: Option<String>,
    pub request_id: Option<String>,
    pub input_tokens: EngineRuntimeUsageV2TokenValue,
    pub output_tokens: EngineRuntimeUsageV2TokenValue,
    pub cache_creation_input_tokens: EngineRuntimeUsageV2TokenValue,
    pub cache_read_input_tokens: EngineRuntimeUsageV2TokenValue,
    pub model: Option<EngineRuntimeUsageV2TextValue>,
    pub effort: Option<EngineRuntimeUsageV2TextValue>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub observed_at_unix_ms: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<RuntimeUsageV2Response> for EngineRuntimeUsageV2Response {
    fn from(value: RuntimeUsageV2Response) -> Self {
        Self {
            usage_key: value.usage_key,
            semantic_revision_ref: value.semantic_revision_ref.into(),
            source_record_ref: value.source_record_ref,
            session_ref: value.session_ref.into(),
            actor_run_ref: value.actor_run_ref.into(),
            response_key_base64: value.response_key_base64,
            response_identity: value.response_identity,
            native_message_id: value.native_message_id,
            request_id: value.request_id,
            input_tokens: value.input_tokens.into(),
            output_tokens: value.output_tokens.into(),
            cache_creation_input_tokens: value.cache_creation_input_tokens.into(),
            cache_read_input_tokens: value.cache_read_input_tokens.into(),
            model: value.model.map(Into::into),
            effort: value.effort.map(Into::into),
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2Affiliation {
    pub affiliation_ref: EngineRuntimeUsageV2ExternalEntityRef,
    pub semantic_revision_ref: EngineRuntimeUsageV2SemanticRevisionRef,
    pub dimension: String,
    pub target_ref: EngineRuntimeUsageV2ExternalEntityRef,
    pub member_ref: Option<EngineRuntimeUsageV2ExternalEntityRef>,
    pub native_target_id: Option<String>,
    pub native_member_id: Option<String>,
    pub state: String,
    pub effective_at: Option<String>,
    pub effective_at_quality: Option<String>,
    pub observed_at_unix_ms: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<RuntimeUsageV2Affiliation> for EngineRuntimeUsageV2Affiliation {
    fn from(value: RuntimeUsageV2Affiliation) -> Self {
        Self {
            affiliation_ref: value.affiliation_ref.into(),
            semantic_revision_ref: value.semantic_revision_ref.into(),
            dimension: value.dimension,
            target_ref: value.target_ref.into(),
            member_ref: value.member_ref.map(Into::into),
            native_target_id: value.native_target_id,
            native_member_id: value.native_member_id,
            state: value.state,
            effective_at: value.effective_at,
            effective_at_quality: value.effective_at_quality,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2ActorContext {
    pub actor_run_ref: EngineRuntimeUsageV2ExternalEntityRef,
    pub semantic_revision_ref: EngineRuntimeUsageV2SemanticRevisionRef,
    pub session_ref: EngineRuntimeUsageV2ExternalEntityRef,
    pub role: String,
    pub parent_actor_run_ref: Option<EngineRuntimeUsageV2ExternalEntityRef>,
    pub native_session_id: Option<String>,
    pub native_actor_id: Option<String>,
    pub native_actor_type: Option<String>,
    pub affiliations: Vec<EngineRuntimeUsageV2Affiliation>,
    pub observed_at_unix_ms: f64,
    pub source_generation: f64,
    pub last_commit_seq: f64,
}

impl From<RuntimeUsageV2ActorContext> for EngineRuntimeUsageV2ActorContext {
    fn from(value: RuntimeUsageV2ActorContext) -> Self {
        Self {
            actor_run_ref: value.actor_run_ref.into(),
            semantic_revision_ref: value.semantic_revision_ref.into(),
            session_ref: value.session_ref.into(),
            role: value.role,
            parent_actor_run_ref: value.parent_actor_run_ref.map(Into::into),
            native_session_id: value.native_session_id,
            native_actor_id: value.native_actor_id,
            native_actor_type: value.native_actor_type,
            affiliations: value.affiliations.into_iter().map(Into::into).collect(),
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_generation: value.source_generation as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2BucketAggregate {
    pub known_tokens: f64,
    pub known_response_count: f64,
    pub exact_response_count: f64,
    pub non_exact_response_count: f64,
    pub unknown_response_count: f64,
    pub completeness: String,
}

impl From<RuntimeUsageV2BucketAggregate> for EngineRuntimeUsageV2BucketAggregate {
    fn from(value: RuntimeUsageV2BucketAggregate) -> Self {
        Self {
            known_tokens: value.known_tokens as f64,
            known_response_count: value.known_response_count as f64,
            exact_response_count: value.exact_response_count as f64,
            non_exact_response_count: value.non_exact_response_count as f64,
            unknown_response_count: value.unknown_response_count as f64,
            completeness: value.completeness.to_string(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2Aggregate {
    pub response_count: f64,
    pub actor_count: f64,
    pub input_tokens: EngineRuntimeUsageV2BucketAggregate,
    pub output_tokens: EngineRuntimeUsageV2BucketAggregate,
    pub cache_creation_input_tokens: EngineRuntimeUsageV2BucketAggregate,
    pub cache_read_input_tokens: EngineRuntimeUsageV2BucketAggregate,
}

impl From<RuntimeUsageV2Aggregate> for EngineRuntimeUsageV2Aggregate {
    fn from(value: RuntimeUsageV2Aggregate) -> Self {
        Self {
            response_count: value.response_count as f64,
            actor_count: value.actor_count as f64,
            input_tokens: value.input_tokens.into(),
            output_tokens: value.output_tokens.into(),
            cache_creation_input_tokens: value.cache_creation_input_tokens.into(),
            cache_read_input_tokens: value.cache_read_input_tokens.into(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2ProjectionReadiness {
    pub projection_id: String,
    pub desired_version: u32,
    pub completed_version: Option<u32>,
    pub state: String,
    pub last_commit_seq: Option<f64>,
    pub updated_at_unix_ms: Option<f64>,
    pub detail: Option<String>,
}

impl From<RuntimeUsageV2ProjectionReadiness> for EngineRuntimeUsageV2ProjectionReadiness {
    fn from(value: RuntimeUsageV2ProjectionReadiness) -> Self {
        Self {
            projection_id: value.projection_id,
            desired_version: value.desired_version,
            completed_version: value.completed_version,
            state: value.state,
            last_commit_seq: value.last_commit_seq.map(|value| value as f64),
            updated_at_unix_ms: value.updated_at_unix_ms.map(|value| value as f64),
            detail: value.detail,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeUsageV2Page {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub projection_status: String,
    pub projection_readiness: EngineRuntimeUsageV2ProjectionReadiness,
    pub project_id: String,
    pub session_id: String,
    pub session_ref: Option<EngineRuntimeUsageV2ExternalEntityRef>,
    pub actor_run_ref: Option<String>,
    pub affiliation_dimension: Option<String>,
    pub affiliation_target_ref: Option<String>,
    pub aggregate: EngineRuntimeUsageV2Aggregate,
    pub items: Vec<EngineRuntimeUsageV2Response>,
    pub actors: Vec<EngineRuntimeUsageV2ActorContext>,
    pub next_cursor: Option<String>,
}

impl From<RuntimeUsageV2Page> for EngineRuntimeUsageV2Page {
    fn from(value: RuntimeUsageV2Page) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            projection_status: value.projection_status,
            projection_readiness: value.projection_readiness.into(),
            project_id: value.project_id,
            session_id: value.session_id,
            session_ref: value.session_ref.map(Into::into),
            actor_run_ref: value.actor_run_ref,
            affiliation_dimension: value.affiliation_dimension,
            affiliation_target_ref: value.affiliation_target_ref,
            aggregate: value.aggregate.into(),
            items: value.items.into_iter().map(Into::into).collect(),
            actors: value.actors.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeSnapshotOptions {
    /// Optional opaque project identity. When omitted, orphan presence/run
    /// evidence remains visible rather than being silently dropped.
    pub project_id: Option<String>,
    /// Optional opaque session identity. With `projectId`, membership is
    /// validated before querying.
    pub session_id: Option<String>,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeRunEvidence {
    pub evidence_id: String,
    pub kind: String,
    pub strength: String,
    pub native_state: Option<String>,
    pub source_time: Option<String>,
    pub source_time_quality: Option<String>,
    pub observed_at_unix_ms: f64,
    pub source_object_id: f64,
    pub last_commit_seq: f64,
}

impl From<RuntimeRunEvidence> for EngineRuntimeRunEvidence {
    fn from(value: RuntimeRunEvidence) -> Self {
        Self {
            evidence_id: value.evidence_id,
            kind: value.kind,
            strength: value.strength,
            native_state: value.native_state,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            source_object_id: value.source_object_id as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeRun {
    pub run_id: String,
    pub session_id: String,
    pub project_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_run_id: String,
    pub parent_run_id: Option<String>,
    pub native_session_id: Option<String>,
    pub native_project_key: Option<String>,
    pub session_present: bool,
    pub state: Option<String>,
    pub decisive_evidence: Option<EngineRuntimeRunEvidence>,
    pub evidence_count: f64,
    pub last_activity_at: Option<String>,
    pub terminal_at: Option<String>,
    pub presence_count: f64,
    pub conflicting_presence_count: f64,
    pub last_commit_seq: f64,
}

impl From<RuntimeRunSnapshot> for EngineRuntimeRun {
    fn from(value: RuntimeRunSnapshot) -> Self {
        Self {
            run_id: value.run_id,
            session_id: value.session_id,
            project_id: value.project_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_run_id: value.native_run_id,
            parent_run_id: value.parent_run_id,
            native_session_id: value.native_session_id,
            native_project_key: value.native_project_key,
            session_present: value.session_present,
            state: value.state,
            decisive_evidence: value.decisive_evidence.map(Into::into),
            evidence_count: value.evidence_count as f64,
            last_activity_at: value.last_activity_at,
            terminal_at: value.terminal_at,
            presence_count: value.presence_count as f64,
            conflicting_presence_count: value.conflicting_presence_count as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimePresence {
    pub presence_id: String,
    pub session_id: String,
    pub run_id: String,
    pub project_id: Option<String>,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_session_id: String,
    pub native_pid: u32,
    pub cwd: String,
    pub started_at: String,
    pub started_at_quality: String,
    pub native_kind: Option<String>,
    pub entrypoint: Option<String>,
    pub name: Option<String>,
    pub native_status: Option<String>,
    pub updated_at: Option<String>,
    pub updated_at_quality: Option<String>,
    pub status_updated_at: Option<String>,
    pub status_updated_at_quality: Option<String>,
    pub native_process_started_at: Option<String>,
    pub version: Option<String>,
    pub peer_protocol: Option<u32>,
    pub name_source: Option<String>,
    pub bridge_session_id: Option<String>,
    pub messaging_socket_path: Option<String>,
    pub presence_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_assertion_count: f64,
    pub observed_at_unix_ms: f64,
    pub session_present: bool,
    pub run_present: bool,
    pub last_commit_seq: f64,
}

impl From<RuntimePresenceSnapshot> for EngineRuntimePresence {
    fn from(value: RuntimePresenceSnapshot) -> Self {
        Self {
            presence_id: value.presence_id,
            session_id: value.session_id,
            run_id: value.run_id,
            project_id: value.project_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_session_id: value.native_session_id,
            native_pid: value.native_pid,
            cwd: value.cwd,
            started_at: value.started_at,
            started_at_quality: value.started_at_quality,
            native_kind: value.native_kind,
            entrypoint: value.entrypoint,
            name: value.name,
            native_status: value.native_status,
            updated_at: value.updated_at,
            updated_at_quality: value.updated_at_quality,
            status_updated_at: value.status_updated_at,
            status_updated_at_quality: value.status_updated_at_quality,
            native_process_started_at: value.native_process_started_at,
            version: value.version,
            peer_protocol: value.peer_protocol,
            name_source: value.name_source,
            bridge_session_id: value.bridge_session_id,
            messaging_socket_path: value.messaging_socket_path,
            presence_status: value.presence_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_assertion_count: value.competing_assertion_count as f64,
            observed_at_unix_ms: value.observed_at_unix_ms as f64,
            session_present: value.session_present,
            run_present: value.run_present,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeEntry {
    pub kind: String,
    pub run: Option<EngineRuntimeRun>,
    pub presence: Option<EngineRuntimePresence>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRuntimeSnapshot {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub entries: Vec<EngineRuntimeEntry>,
    pub next_cursor: Option<String>,
}

impl From<RuntimeSnapshot> for EngineRuntimeSnapshot {
    fn from(value: RuntimeSnapshot) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            project_id: value.project_id,
            session_id: value.session_id,
            entries: value
                .entries
                .into_iter()
                .map(|entry| EngineRuntimeEntry {
                    kind: entry.kind,
                    run: entry.run.map(Into::into),
                    presence: entry.presence.map(Into::into),
                })
                .collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineRunStateLookup {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub run: Option<EngineRuntimeRun>,
}

impl From<RunStateLookup> for EngineRunStateLookup {
    fn from(value: RunStateLookup) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            run: value.run.map(Into::into),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamPageOptions {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamScopedPageOptions {
    pub team_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamInboxMessagePageOptions {
    pub inbox_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamConfig {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub created_at_quality: String,
    pub lead_member_id: Option<String>,
    pub lead_member_present: bool,
    pub native_lead_agent_id: String,
    pub lead_session_id: String,
    pub lead_session_present: bool,
    pub native_lead_session_id: String,
    pub config_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_snapshot_count: f64,
    pub member_count: f64,
    pub last_commit_seq: f64,
}

impl From<TeamConfigSummary> for EngineTeamConfig {
    fn from(value: TeamConfigSummary) -> Self {
        Self {
            name: value.name,
            description: value.description,
            created_at: value.created_at,
            created_at_quality: value.created_at_quality,
            lead_member_id: value.lead_member_id,
            lead_member_present: value.lead_member_present,
            native_lead_agent_id: value.native_lead_agent_id,
            lead_session_id: value.lead_session_id,
            lead_session_present: value.lead_session_present,
            native_lead_session_id: value.native_lead_session_id,
            config_status: value.config_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_snapshot_count: value.competing_snapshot_count as f64,
            member_count: value.member_count as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamSummary {
    pub team_id: String,
    pub adapter_id: String,
    pub source_instance_id: f64,
    pub native_team_id: String,
    pub config: Option<EngineTeamConfig>,
    pub inbox_count: f64,
    pub message_count: f64,
    pub unread_message_count: f64,
    pub conflicting_inbox_count: f64,
    pub conflicting_message_count: f64,
    pub last_commit_seq: f64,
}

impl From<TeamSummary> for EngineTeamSummary {
    fn from(value: TeamSummary) -> Self {
        Self {
            team_id: value.team_id,
            adapter_id: value.adapter_id,
            source_instance_id: value.source_instance_id as f64,
            native_team_id: value.native_team_id,
            config: value.config.map(Into::into),
            inbox_count: value.inbox_count as f64,
            message_count: value.message_count as f64,
            unread_message_count: value.unread_message_count as f64,
            conflicting_inbox_count: value.conflicting_inbox_count as f64,
            conflicting_message_count: value.conflicting_message_count as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub items: Vec<EngineTeamSummary>,
    pub next_cursor: Option<String>,
}

impl From<TeamPage> for EngineTeamPage {
    fn from(value: TeamPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamMember {
    pub member_id: String,
    pub team_id: String,
    pub member_ordinal: u32,
    pub native_agent_id: String,
    pub native_name: String,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub color: Option<String>,
    pub plan_mode_required: Option<bool>,
    pub joined_at: String,
    pub joined_at_quality: String,
    pub tmux_pane_id: String,
    pub cwd: String,
    pub subscriptions: Vec<String>,
    pub backend_type: Option<String>,
    pub membership_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_membership_count: f64,
    pub last_commit_seq: f64,
}

impl From<TeamMember> for EngineTeamMember {
    fn from(value: TeamMember) -> Self {
        Self {
            member_id: value.member_id,
            team_id: value.team_id,
            member_ordinal: value.member_ordinal,
            native_agent_id: value.native_agent_id,
            native_name: value.native_name,
            agent_type: value.agent_type,
            model: value.model,
            prompt: value.prompt,
            color: value.color,
            plan_mode_required: value.plan_mode_required,
            joined_at: value.joined_at,
            joined_at_quality: value.joined_at_quality,
            tmux_pane_id: value.tmux_pane_id,
            cwd: value.cwd,
            subscriptions: value.subscriptions,
            backend_type: value.backend_type,
            membership_status: value.membership_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_membership_count: value.competing_membership_count as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamDetails {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub team: EngineTeamSummary,
    pub members: Vec<EngineTeamMember>,
}

impl From<TeamDetails> for EngineTeamDetails {
    fn from(value: TeamDetails) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            team: value.team.into(),
            members: value.members.into_iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamInbox {
    pub inbox_id: String,
    pub team_id: String,
    pub recipient_id: String,
    pub recipient_present: bool,
    pub native_team_id: String,
    pub native_recipient_name: String,
    pub inbox_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_snapshot_count: f64,
    pub message_count: f64,
    pub unread_message_count: f64,
    pub conflicting_message_count: f64,
    pub last_commit_seq: f64,
}

impl From<TeamInboxSummary> for EngineTeamInbox {
    fn from(value: TeamInboxSummary) -> Self {
        Self {
            inbox_id: value.inbox_id,
            team_id: value.team_id,
            recipient_id: value.recipient_id,
            recipient_present: value.recipient_present,
            native_team_id: value.native_team_id,
            native_recipient_name: value.native_recipient_name,
            inbox_status: value.inbox_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_snapshot_count: value.competing_snapshot_count as f64,
            message_count: value.message_count as f64,
            unread_message_count: value.unread_message_count as f64,
            conflicting_message_count: value.conflicting_message_count as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamInboxPage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub team_id: String,
    pub items: Vec<EngineTeamInbox>,
    pub next_cursor: Option<String>,
}

impl From<TeamInboxPage> for EngineTeamInboxPage {
    fn from(value: TeamInboxPage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            team_id: value.team_id,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamInboxMessage {
    pub message_id: String,
    pub inbox_id: String,
    pub sender_id: String,
    pub sender_present: bool,
    pub message_ordinal: u32,
    pub native_message_id: Option<String>,
    pub native_kind: Option<String>,
    pub native_version: Option<u32>,
    pub native_sender_name: String,
    pub text: String,
    pub summary: Option<String>,
    pub color: Option<String>,
    pub source_time: String,
    pub source_time_quality: String,
    pub read: bool,
    pub message_status: String,
    pub decisive_fact_id: String,
    pub assertion_count: f64,
    pub competing_message_count: f64,
    pub last_commit_seq: f64,
}

impl From<TeamInboxMessage> for EngineTeamInboxMessage {
    fn from(value: TeamInboxMessage) -> Self {
        Self {
            message_id: value.message_id,
            inbox_id: value.inbox_id,
            sender_id: value.sender_id,
            sender_present: value.sender_present,
            message_ordinal: value.message_ordinal,
            native_message_id: value.native_message_id,
            native_kind: value.native_kind,
            native_version: value.native_version,
            native_sender_name: value.native_sender_name,
            text: value.text,
            summary: value.summary,
            color: value.color,
            source_time: value.source_time,
            source_time_quality: value.source_time_quality,
            read: value.read,
            message_status: value.message_status,
            decisive_fact_id: value.decisive_fact_id,
            assertion_count: value.assertion_count as f64,
            competing_message_count: value.competing_message_count as f64,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineTeamInboxMessagePage {
    pub contract_version: u32,
    pub at_commit_seq: f64,
    pub inbox_id: String,
    pub team_id: String,
    pub native_team_id: String,
    pub native_recipient_name: String,
    pub items: Vec<EngineTeamInboxMessage>,
    pub next_cursor: Option<String>,
}

impl From<TeamInboxMessagePage> for EngineTeamInboxMessagePage {
    fn from(value: TeamInboxMessagePage) -> Self {
        Self {
            contract_version: value.contract_version,
            at_commit_seq: value.at_commit_seq as f64,
            inbox_id: value.inbox_id,
            team_id: value.team_id,
            native_team_id: value.native_team_id,
            native_recipient_name: value.native_recipient_name,
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineReconcileOptions {
    /// Configured native data roots understood by the selected adapter.
    pub roots: Vec<String>,
    /// Durable ingest reason. Defaults to `manual_reconcile`.
    pub reason: Option<String>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineAdapterReconcileOptions {
    /// Open adapter identifier registered by the native composition root.
    pub adapter_id: String,
    /// Configured native data roots understood by that adapter.
    pub roots: Vec<String>,
    /// Durable ingest reason. Defaults to `manual_reconcile`.
    pub reason: Option<String>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineObservationOptions {
    /// Configured native data roots understood by the selected adapter.
    pub roots: Vec<String>,
    /// Durable ingest reason prefix. Defaults to `native_watch`.
    pub reason: Option<String>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineAdapterObservationOptions {
    /// Open adapter identifier registered by the native composition root.
    pub adapter_id: String,
    /// Configured native data roots understood by that adapter.
    pub roots: Vec<String>,
    /// Durable ingest reason prefix. Defaults to `native_watch`.
    pub reason: Option<String>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineReconcileResult {
    pub instances_discovered: u32,
    pub streams_reconciled: u32,
    pub streams_unavailable: u32,
    pub objects_discovered: u32,
    pub objects_registered: u32,
    pub objects_changed: u32,
    pub objects_unchanged: u32,
    pub objects_removed: u32,
    pub records_decoded: u32,
    pub records_quarantined: u32,
    pub retries_required: u32,
    pub incomplete_tail_retries: u32,
    pub dependency_access_attempts: f64,
    pub dependency_access_denials: f64,
    pub dependency_access_abandoned: f64,
    pub dependency_objects_accessed: f64,
    pub dependency_bytes_read: f64,
    pub dependency_rows_read: f64,
    pub dependency_max_depth: u32,
    pub dependency_trace_entries_dropped: f64,
    pub commits: u32,
    pub last_commit_seq: Option<f64>,
}

impl From<ReconcileOutcome> for EngineReconcileResult {
    fn from(value: ReconcileOutcome) -> Self {
        Self {
            instances_discovered: value.instances_discovered,
            streams_reconciled: value.streams_reconciled,
            streams_unavailable: value.streams_unavailable,
            objects_discovered: value.objects_discovered,
            objects_registered: value.objects_registered,
            objects_changed: value.objects_changed,
            objects_unchanged: value.objects_unchanged,
            objects_removed: value.objects_removed,
            records_decoded: value.records_decoded,
            records_quarantined: value.records_quarantined,
            retries_required: value.retries_required,
            incomplete_tail_retries: value.incomplete_tail_retries,
            dependency_access_attempts: value.dependency_access_attempts as f64,
            dependency_access_denials: value.dependency_access_denials as f64,
            dependency_access_abandoned: value.dependency_access_abandoned as f64,
            dependency_objects_accessed: value.dependency_objects_accessed as f64,
            dependency_bytes_read: value.dependency_bytes_read as f64,
            dependency_rows_read: value.dependency_rows_read as f64,
            dependency_max_depth: value.dependency_max_depth,
            dependency_trace_entries_dropped: value.dependency_trace_entries_dropped as f64,
            commits: value.commits,
            last_commit_seq: value.last_commit_seq.map(|value| value as f64),
        }
    }
}

impl From<EngineOverview> for EngineOverviewResult {
    fn from(value: EngineOverview) -> Self {
        Self {
            schema_version: value.schema_version,
            commit_seq: value.commit_seq as f64,
            projects: value.projects,
            sessions: value.sessions,
            messages: value.messages,
            canonical_sessions: value.canonical_sessions,
            canonical_messages: value.canonical_messages,
            change_log_oldest_cursor: value.change_log_oldest_cursor.map(Into::into),
            change_log_pruned_through_seq: value.change_log_pruned_through_seq as f64,
            change_log_retained_changes: value.change_log_retained_changes as f64,
            change_log_retained_payload_bytes: value.change_log_retained_payload_bytes as f64,
            writer_data_version: value.writer_data_version,
            journal_mode: value.journal_mode,
            query_only: value.query_only,
            read_only: value.read_only,
        }
    }
}

/// Persistent RFC 011 engine handle. Construct with
/// [`open_spaghetti_engine`]; explicit `dispose()` is preferred, with Rust
/// finalization retaining a last-resort cleanup path.
#[napi]
pub struct SpaghettiEngine {
    inner: Arc<SpaghettiEngineCore>,
}

#[napi]
impl SpaghettiEngine {
    /// Construction is intentionally async through `openSpaghettiEngine`.
    /// The impossible TypeScript argument prevents an implicit public
    /// zero-argument constructor from appearing in the generated declaration.
    #[napi(constructor, ts_args_type = "_notConstructible: never")]
    pub fn unsupported_constructor() -> Result<Self> {
        Err(Error::new(
            Status::InvalidArg,
            "SpaghettiEngine cannot be constructed directly; use openSpaghettiEngine(options)",
        ))
    }

    #[napi(getter)]
    pub fn status(&self) -> EngineStatus {
        self.inner.status().into()
    }

    /// Probe the writer and one query worker off the JavaScript thread.
    #[napi(ts_return_type = "Promise<EngineHealth>")]
    pub fn health(&self, signal: Option<AbortSignal>) -> AsyncTask<HealthTask> {
        AsyncTask::with_optional_signal(
            HealthTask {
                engine: Arc::clone(&self.inner),
            },
            signal,
        )
    }

    /// Execute the first typed, read-only Rust query.
    #[napi(ts_return_type = "Promise<EngineOverviewResult>")]
    pub fn overview(&self, signal: Option<AbortSignal>) -> AsyncTask<OverviewTask> {
        AsyncTask::with_optional_signal(
            OverviewTask {
                engine: Arc::clone(&self.inner),
            },
            signal,
        )
    }

    /// Replay one bounded, snapshot-consistent page of durable projection
    /// changes. Binary keys and payloads remain lossless base64 strings.
    #[napi(ts_return_type = "Promise<EngineChangeReplay>")]
    pub fn replay_changes(
        &self,
        options: Option<EngineChangeReplayOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<ChangeReplayTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            ChangeReplayTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Wait off the JavaScript thread for the Rust writer to publish a newer
    /// durable commit. No SQLite read is performed while the request is idle.
    #[napi(ts_return_type = "Promise<EngineCommitWaitResult>")]
    pub fn wait_for_commit(
        &self,
        env: Env,
        options: EngineCommitWaitOptions,
        signal: Option<AbortSignal>,
    ) -> Result<AsyncBlock<EngineCommitWaitResult>> {
        const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
        if !options.after_commit_seq.is_finite()
            || options.after_commit_seq.fract() != 0.0
            || !(0.0..=MAX_SAFE_INTEGER).contains(&options.after_commit_seq)
        {
            return Err(Error::new(
                Status::InvalidArg,
                "afterCommitSeq must be a non-negative safe integer",
            ));
        }
        let engine = Arc::clone(&self.inner);
        let after_commit_seq = options.after_commit_seq as u64;
        let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_COMMIT_WAIT_TIMEOUT_MS);
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncBlockBuilder::new(async move {
            engine
                .wait_for_commit(after_commit_seq, timeout_ms, cancellation)
                .await
                .map(Into::into)
                .map_err(napi_error)
        })
        .build(&env)
    }

    /// List canonical projects in Rust-defined activity order. The cursor is
    /// opaque, versioned, and valid only for this query.
    #[napi(ts_return_type = "Promise<EngineHistoryProjectPage>")]
    pub fn list_history_projects(
        &self,
        options: Option<EngineHistoryPageOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<HistoryProjectsTask> {
        AsyncTask::with_optional_signal(
            HistoryProjectsTask {
                engine: Arc::clone(&self.inner),
                options,
            },
            signal,
        )
    }

    /// List transcript-backed sessions for one canonical project. Native
    /// session-index metadata is returned as explicitly sourced enrichment.
    #[napi(ts_return_type = "Promise<EngineHistorySessionPage>")]
    pub fn list_history_sessions(
        &self,
        options: EngineHistorySessionPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<HistorySessionsTask> {
        AsyncTask::with_optional_signal(
            HistorySessionsTask {
                engine: Arc::clone(&self.inner),
                options,
            },
            signal,
        )
    }

    /// Read one transcript-backed canonical session and its projection counts.
    /// A well-formed unknown identity returns an absent `session`.
    #[napi(ts_return_type = "Promise<EngineSessionDetails>")]
    pub fn get_session(
        &self,
        session_id: String,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<SessionDetailsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            SessionDetailsTask {
                engine: Arc::clone(&self.inner),
                session_id,
                cancellation,
            },
            signal,
        )
    }

    /// Page canonical messages for one verified project/session membership.
    /// Both row count and decoded JSON payload bytes are bounded in Rust.
    #[napi(ts_return_type = "Promise<EngineMessagePage>")]
    pub fn get_messages(
        &self,
        options: EngineMessagePageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<MessagesTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            MessagesTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Search all canonical root and delegated messages in one FTS score
    /// domain. Exact totals, filtering, snippets, and paging are Rust-owned.
    #[napi(ts_return_type = "Promise<EngineSearchPage>")]
    pub fn search(
        &self,
        options: EngineSearchPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<SearchTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            SearchTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Read one root-and-delegated canonical timeline page plus exact session
    /// facets in a single SQLite snapshot.
    #[napi(ts_return_type = "Promise<EngineTimelinePage>")]
    pub fn get_timeline(
        &self,
        options: EngineTimelinePageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<TimelineTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            TimelineTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page current child-run delegation relations for one canonical session.
    #[napi(ts_return_type = "Promise<EngineDelegationPage>")]
    pub fn list_delegations(
        &self,
        options: EngineDelegationPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<DelegationsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            DelegationsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page canonical workflow containers for one canonical session.
    #[napi(ts_return_type = "Promise<EngineWorkflowPage>")]
    pub fn list_workflows(
        &self,
        options: EngineWorkflowPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<WorkflowsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            WorkflowsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Read one workflow container and its bounded native snapshot.
    #[napi(ts_return_type = "Promise<EngineWorkflowDetails>")]
    pub fn get_workflow(
        &self,
        workflow_id: String,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<WorkflowDetailsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            WorkflowDetailsTask {
                engine: Arc::clone(&self.inner),
                workflow_id,
                cancellation,
            },
            signal,
        )
    }

    /// Page native workflow members and their explicit journal evidence.
    #[napi(ts_return_type = "Promise<EngineWorkflowMemberPage>")]
    pub fn list_workflow_members(
        &self,
        options: EngineWorkflowMemberPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<WorkflowMembersTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            WorkflowMembersTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page canonical project-memory documents. Exact UTF-8 content and row
    /// count are bounded in Rust.
    #[napi(ts_return_type = "Promise<EngineMemoryDocumentPage>")]
    pub fn list_memory_documents(
        &self,
        options: EngineMemoryDocumentPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<MemoryDocumentsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            MemoryDocumentsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page canonical task collections globally or under one trusted scope.
    #[napi(ts_return_type = "Promise<EngineTaskCollectionPage>")]
    pub fn list_task_collections(
        &self,
        options: Option<EngineTaskCollectionPageOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<TaskCollectionsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            TaskCollectionsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page canonical task items for one opaque collection identity.
    #[napi(ts_return_type = "Promise<EngineTaskPage>")]
    pub fn list_tasks(
        &self,
        options: EngineTaskPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<TasksTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            TasksTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page global plan documents. No session relation is fabricated.
    #[napi(ts_return_type = "Promise<EnginePlanPage>")]
    pub fn list_plans(
        &self,
        options: Option<EngineCapabilityPageOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<PlansTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            PlansTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page persisted tool-result sidecars for one verified session.
    #[napi(ts_return_type = "Promise<EngineToolResultPage>")]
    pub fn list_tool_results(
        &self,
        options: EngineToolResultPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<ToolResultsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            ToolResultsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page session-scoped file-history artifacts. Arbitrary content is
    /// represented as base64 and bounded by Rust before crossing N-API.
    #[napi(ts_return_type = "Promise<EngineArtifactPage>")]
    pub fn list_artifacts(
        &self,
        options: EngineArtifactPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<ArtifactsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            ArtifactsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// List configured source instances and their durable ingest inventory.
    #[napi(ts_return_type = "Promise<EngineSourcePage>")]
    pub fn list_sources(
        &self,
        options: Option<EngineHistoryPageOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<SourcesTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            SourcesTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Return one snapshot-consistent set of canonical and source-catalog
    /// counts. Compatibility-cache tables are intentionally excluded.
    #[napi(ts_return_type = "Promise<EngineCanonicalStats>")]
    pub fn get_stats(&self, signal: Option<AbortSignal>) -> AsyncTask<CanonicalStatsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            CanonicalStatsTask {
                engine: Arc::clone(&self.inner),
                cancellation,
            },
            signal,
        )
    }

    /// Return canonical usage totals for one project or one verified session.
    #[napi(ts_return_type = "Promise<EngineUsageTotals>")]
    pub fn get_usage(
        &self,
        options: EngineUsageScopeOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<UsageTotalsTask> {
        let cancellation = QueryCancellationToken::default();
        if let Some(signal) = signal.as_ref() {
            let abort_cancellation = cancellation.clone();
            signal.on_abort(move || abort_cancellation.cancel());
        }
        AsyncTask::with_optional_signal(
            UsageTotalsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Return inclusive daily usage activity and separately surfaced untimed
    /// contributions for one canonical project/session scope.
    #[napi(ts_return_type = "Promise<EngineUsageActivity>")]
    pub fn get_usage_activity(
        &self,
        options: EngineUsageActivityOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<UsageActivityTask> {
        let cancellation = QueryCancellationToken::default();
        if let Some(signal) = signal.as_ref() {
            let abort_cancellation = cancellation.clone();
            signal.on_abort(move || abort_cancellation.cancel());
        }
        AsyncTask::with_optional_signal(
            UsageActivityTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page canonical response-level usage revisions and their current actor
    /// and affiliation context. This is an explicitly shadow-only RFC 012C
    /// surface; legacy additive usage queries remain unchanged.
    #[napi(ts_return_type = "Promise<EngineRuntimeUsageV2Page>")]
    pub fn get_runtime_usage_v2(
        &self,
        options: EngineRuntimeUsageV2Options,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<RuntimeUsageV2Task> {
        let cancellation = QueryCancellationToken::default();
        if let Some(signal) = signal.as_ref() {
            let abort_cancellation = cancellation.clone();
            signal.on_abort(move || abort_cancellation.cancel());
        }
        AsyncTask::with_optional_signal(
            RuntimeUsageV2Task {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page normalized RFC 012A coverage for one fact family using opaque
    /// common identities. The result shares one durable commit watermark and
    /// never exposes native paths or object keys.
    #[napi(ts_return_type = "Promise<EngineFactFamilyCoveragePage>")]
    pub fn get_fact_family_coverage(
        &self,
        options: EngineFactFamilyCoverageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<FactFamilyCoverageTask> {
        let cancellation = QueryCancellationToken::default();
        if let Some(signal) = signal.as_ref() {
            let abort_cancellation = cancellation.clone();
            signal.on_abort(move || abort_cancellation.cancel());
        }
        AsyncTask::with_optional_signal(
            FactFamilyCoverageTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Return durable run-state and current registry-presence evidence. This
    /// intentionally does not probe PIDs or synthesize freshness assessments.
    #[napi(ts_return_type = "Promise<EngineRuntimeSnapshot>")]
    pub fn get_runtime_snapshot(
        &self,
        options: Option<EngineRuntimeSnapshotOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<RuntimeSnapshotTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            RuntimeSnapshotTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Look up one canonical run without probing process liveness. A
    /// well-formed unknown identity returns an absent `run`.
    #[napi(ts_return_type = "Promise<EngineRunStateLookup>")]
    pub fn get_run_state(
        &self,
        run_id: String,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<RunStateTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            RunStateTask {
                engine: Arc::clone(&self.inner),
                run_id,
                cancellation,
            },
            signal,
        )
    }

    /// List current canonical teams, including inbox-only team identities.
    #[napi(ts_return_type = "Promise<EngineTeamPage>")]
    pub fn list_teams(
        &self,
        options: Option<EngineTeamPageOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<TeamsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            TeamsTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Read one current team configuration and its bounded member snapshot.
    #[napi(ts_return_type = "Promise<EngineTeamDetails>")]
    pub fn get_team(
        &self,
        team_id: String,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<TeamDetailsTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            TeamDetailsTask {
                engine: Arc::clone(&self.inner),
                team_id,
                cancellation,
            },
            signal,
        )
    }

    /// Page inbox summaries without returning potentially sensitive message
    /// bodies in a directory listing.
    #[napi(ts_return_type = "Promise<EngineTeamInboxPage>")]
    pub fn list_team_inboxes(
        &self,
        options: EngineTeamScopedPageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<TeamInboxesTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            TeamInboxesTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Page one inbox's messages in native snapshot order.
    #[napi(ts_return_type = "Promise<EngineTeamInboxMessagePage>")]
    pub fn list_team_inbox_messages(
        &self,
        options: EngineTeamInboxMessagePageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<TeamInboxMessagesTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            TeamInboxMessagesTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Reconcile any registered adapter through the common Rust source and
    /// projection transaction path.
    #[napi(ts_return_type = "Promise<EngineReconcileResult>")]
    pub fn reconcile_adapter(
        &self,
        options: EngineAdapterReconcileOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<ReconcileClaudeTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            ReconcileClaudeTask {
                engine: Arc::clone(&self.inner),
                adapter_id: options.adapter_id,
                options: EngineReconcileOptions {
                    roots: options.roots,
                    reason: options.reason,
                },
                cancellation,
            },
            signal,
        )
    }

    /// Register consolidated roots and supervise any registered adapter.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn start_observation(
        &self,
        options: EngineAdapterObservationOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<StartClaudeObservationTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            StartClaudeObservationTask {
                engine: Arc::clone(&self.inner),
                adapter_id: options.adapter_id,
                options: EngineObservationOptions {
                    roots: options.roots,
                    reason: options.reason,
                },
                cancellation,
            },
            signal,
        )
    }

    /// Force one running adapter supervisor through common reconciliation.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn refresh_observation(
        &self,
        adapter_id: String,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<RefreshClaudeObservationTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            RefreshClaudeObservationTask {
                engine: Arc::clone(&self.inner),
                adapter_id,
                cancellation,
            },
            signal,
        )
    }

    /// Stop one adapter supervisor without disposing the engine.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn stop_observation(
        &self,
        adapter_id: String,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<StopClaudeObservationTask> {
        AsyncTask::with_optional_signal(
            StopClaudeObservationTask {
                engine: Arc::clone(&self.inner),
                adapter_id,
            },
            signal,
        )
    }

    /// Reconcile the adapter-declared Claude source map through the common
    /// Rust drivers, decoders, projections, and durable cursor transaction.
    #[napi(ts_return_type = "Promise<EngineReconcileResult>")]
    pub fn reconcile_claude(
        &self,
        options: EngineReconcileOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<ReconcileClaudeTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            ReconcileClaudeTask {
                engine: Arc::clone(&self.inner),
                adapter_id: CLAUDE_ADAPTER_ID.to_string(),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Register consolidated native roots before an initial scan, then keep
    /// one bounded Rust supervisor reconciling Claude changes and polling.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn start_claude_observation(
        &self,
        options: EngineObservationOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<StartClaudeObservationTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            StartClaudeObservationTask {
                engine: Arc::clone(&self.inner),
                adapter_id: CLAUDE_ADAPTER_ID.to_string(),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Force the running Claude supervisor through its common reconcile path.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn refresh_claude_observation(
        &self,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<RefreshClaudeObservationTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            RefreshClaudeObservationTask {
                engine: Arc::clone(&self.inner),
                adapter_id: CLAUDE_ADAPTER_ID.to_string(),
                cancellation,
            },
            signal,
        )
    }

    /// Stop native Claude watch registration without disposing the engine.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn stop_claude_observation(
        &self,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<StopClaudeObservationTask> {
        AsyncTask::with_optional_signal(
            StopClaudeObservationTask {
                engine: Arc::clone(&self.inner),
                adapter_id: CLAUDE_ADAPTER_ID.to_string(),
            },
            signal,
        )
    }

    /// Invalidate queued query requests. Requests submitted afterward use a
    /// new cancellation epoch and remain valid.
    #[napi]
    pub fn cancel_pending_queries(&self) -> Result<u32> {
        self.inner
            .cancel_pending_queries()
            .map(|epoch| u32::try_from(epoch).unwrap_or(u32::MAX))
            .map_err(napi_error)
    }

    /// Finalize a size-gated cold bootstrap and admit the read pool only
    /// after indexes, canonical FTS, and integrity checks have converged.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn complete_query_bootstrap(&self) -> AsyncTask<CompleteQueryBootstrapTask> {
        AsyncTask::new(CompleteQueryBootstrapTask {
            engine: Arc::clone(&self.inner),
        })
    }

    /// Deterministically stop readers, stop the writer, and release ownership.
    #[napi(ts_return_type = "Promise<EngineStatus>")]
    pub fn dispose(&self) -> AsyncTask<DisposeTask> {
        AsyncTask::new(DisposeTask {
            engine: Arc::clone(&self.inner),
        })
    }
}

/// Open the persistent engine on a libuv worker thread.
#[napi(ts_return_type = "Promise<SpaghettiEngine>")]
pub fn open_spaghetti_engine(options: EngineOpenOptions) -> AsyncTask<OpenEngineTask> {
    AsyncTask::new(OpenEngineTask { options })
}

pub struct OpenEngineTask {
    options: EngineOpenOptions,
}

pub struct CompleteQueryBootstrapTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl Task for OpenEngineTask {
    type Output = SpaghettiEngine;
    type JsValue = SpaghettiEngine;

    fn compute(&mut self) -> Result<Self::Output> {
        let query_workers = self
            .options
            .query_workers
            .map(|value| usize::try_from(value).unwrap_or(usize::MAX));
        let registry = AdapterRegistry::builder()
            .register(ClaudeCodeAdapter::new())
            .register(CodexAdapter::new())
            .register(GrokAdapter::new())
            .build_legacy()
            .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?;
        let inner = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: PathBuf::from(&self.options.db_path),
                query_workers,
                owner_label: self.options.owner_label.clone(),
                defer_query_structures: self.options.bootstrap_query_structures.unwrap_or(false),
            },
            registry,
        )
        .map_err(napi_error)?;
        Ok(SpaghettiEngine { inner })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for CompleteQueryBootstrapTask {
    type Output = EngineStatus;
    type JsValue = EngineStatus;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine.complete_query_bootstrap().map_err(napi_error)?;
        Ok(self.engine.status().into())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct HealthTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl Task for HealthTask {
    type Output = EngineHealth;
    type JsValue = EngineHealth;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self.engine.health().into())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct OverviewTask {
    engine: Arc<SpaghettiEngineCore>,
}

pub struct ChangeReplayTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineChangeReplayOptions>,
    cancellation: QueryCancellationToken,
}

pub struct HistoryProjectsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineHistoryPageOptions>,
}

pub struct HistorySessionsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineHistorySessionPageOptions,
}

pub struct SessionDetailsTask {
    engine: Arc<SpaghettiEngineCore>,
    session_id: String,
    cancellation: QueryCancellationToken,
}

pub struct MessagesTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineMessagePageOptions,
    cancellation: QueryCancellationToken,
}

pub struct SearchTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineSearchPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct TimelineTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineTimelinePageOptions,
    cancellation: QueryCancellationToken,
}

pub struct DelegationsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineDelegationPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct WorkflowsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineWorkflowPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct WorkflowDetailsTask {
    engine: Arc<SpaghettiEngineCore>,
    workflow_id: String,
    cancellation: QueryCancellationToken,
}

pub struct WorkflowMembersTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineWorkflowMemberPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct MemoryDocumentsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineMemoryDocumentPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct TaskCollectionsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineTaskCollectionPageOptions>,
    cancellation: QueryCancellationToken,
}

pub struct TasksTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineTaskPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct PlansTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineCapabilityPageOptions>,
    cancellation: QueryCancellationToken,
}

pub struct ToolResultsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineToolResultPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct ArtifactsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineArtifactPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct SourcesTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineHistoryPageOptions>,
    cancellation: QueryCancellationToken,
}

pub struct CanonicalStatsTask {
    engine: Arc<SpaghettiEngineCore>,
    cancellation: QueryCancellationToken,
}

pub struct UsageTotalsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineUsageScopeOptions,
    cancellation: QueryCancellationToken,
}

pub struct UsageActivityTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineUsageActivityOptions,
    cancellation: QueryCancellationToken,
}

pub struct RuntimeUsageV2Task {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineRuntimeUsageV2Options,
    cancellation: QueryCancellationToken,
}

pub struct FactFamilyCoverageTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineFactFamilyCoverageOptions,
    cancellation: QueryCancellationToken,
}

pub struct RuntimeSnapshotTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineRuntimeSnapshotOptions>,
    cancellation: QueryCancellationToken,
}

pub struct RunStateTask {
    engine: Arc<SpaghettiEngineCore>,
    run_id: String,
    cancellation: QueryCancellationToken,
}

pub struct TeamsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineTeamPageOptions>,
    cancellation: QueryCancellationToken,
}

pub struct TeamDetailsTask {
    engine: Arc<SpaghettiEngineCore>,
    team_id: String,
    cancellation: QueryCancellationToken,
}

pub struct TeamInboxesTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineTeamScopedPageOptions,
    cancellation: QueryCancellationToken,
}

pub struct TeamInboxMessagesTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineTeamInboxMessagePageOptions,
    cancellation: QueryCancellationToken,
}

pub struct ReconcileClaudeTask {
    engine: Arc<SpaghettiEngineCore>,
    adapter_id: String,
    options: EngineReconcileOptions,
    cancellation: QueryCancellationToken,
}

pub struct StartClaudeObservationTask {
    engine: Arc<SpaghettiEngineCore>,
    adapter_id: String,
    options: EngineObservationOptions,
    cancellation: QueryCancellationToken,
}

pub struct RefreshClaudeObservationTask {
    engine: Arc<SpaghettiEngineCore>,
    adapter_id: String,
    cancellation: QueryCancellationToken,
}

pub struct StopClaudeObservationTask {
    engine: Arc<SpaghettiEngineCore>,
    adapter_id: String,
}

impl Task for ReconcileClaudeTask {
    type Output = EngineReconcileResult;
    type JsValue = EngineReconcileResult;

    fn compute(&mut self) -> Result<Self::Output> {
        validate_roots(&self.options.roots, "reconcileAdapter")?;
        let request = ReconcileRequest {
            configured_roots: self.options.roots.iter().map(PathBuf::from).collect(),
            reason: self
                .options
                .reason
                .clone()
                .unwrap_or_else(|| "manual_reconcile".to_string()),
        };
        self.engine
            .reconcile_adapter_cancellable(&self.adapter_id, request, self.cancellation.clone())
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for StartClaudeObservationTask {
    type Output = EngineStatus;
    type JsValue = EngineStatus;

    fn compute(&mut self) -> Result<Self::Output> {
        validate_roots(&self.options.roots, "startObservation")?;
        let mut options = ObservationSupervisorOptions::new(
            self.options.roots.iter().map(PathBuf::from).collect(),
        );
        if let Some(reason) = self.options.reason.clone() {
            options.reason = reason;
        }
        self.engine
            .start_registered_observation_cancellable(
                &self.adapter_id,
                options,
                self.cancellation.clone(),
            )
            .map_err(napi_error)?;
        Ok(self.engine.status().into())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for RefreshClaudeObservationTask {
    type Output = EngineStatus;
    type JsValue = EngineStatus;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .refresh_observation_supervisor_cancellable(&self.adapter_id, self.cancellation.clone())
            .map_err(napi_error)?;
        Ok(self.engine.status().into())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for StopClaudeObservationTask {
    type Output = EngineStatus;
    type JsValue = EngineStatus;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .stop_observation_supervisor(&self.adapter_id)
            .map_err(napi_error)?;
        Ok(self.engine.status().into())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for OverviewTask {
    type Output = EngineOverviewResult;
    type JsValue = EngineOverviewResult;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine.overview().map(Into::into).map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for ChangeReplayTask {
    type Output = EngineChangeReplay;
    type JsValue = EngineChangeReplay;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self.options.clone().unwrap_or(EngineChangeReplayOptions {
            after: None,
            topics: None,
            limit: None,
        });
        let after = options.after.map(change_cursor_from_js).transpose()?;
        self.engine
            .replay_changes_cancellable(
                ChangeReplayRequest {
                    after,
                    topics: options.topics.unwrap_or_default(),
                    limit: options.limit.unwrap_or(DEFAULT_CHANGE_REPLAY_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for HistoryProjectsTask {
    type Output = EngineHistoryProjectPage;
    type JsValue = EngineHistoryProjectPage;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self.options.clone().unwrap_or(EngineHistoryPageOptions {
            cursor: None,
            limit: None,
        });
        self.engine
            .history_projects(HistoryProjectPageRequest {
                cursor: options.cursor,
                limit: options.limit.unwrap_or(DEFAULT_HISTORY_PAGE_LIMIT),
            })
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for HistorySessionsTask {
    type Output = EngineHistorySessionPage;
    type JsValue = EngineHistorySessionPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .history_sessions(HistorySessionPageRequest {
                project_id: self.options.project_id.clone(),
                cursor: self.options.cursor.clone(),
                limit: self.options.limit.unwrap_or(DEFAULT_HISTORY_PAGE_LIMIT),
            })
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for SessionDetailsTask {
    type Output = EngineSessionDetails;
    type JsValue = EngineSessionDetails;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .session_details_cancellable(
                SessionDetailsRequest {
                    session_id: self.session_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for MessagesTask {
    type Output = EngineMessagePage;
    type JsValue = EngineMessagePage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .messages_cancellable(
                MessagePageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_DETAIL_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for SearchTask {
    type Output = EngineSearchPage;
    type JsValue = EngineSearchPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .search_cancellable(
                SearchPageRequest {
                    text: self.options.text.clone(),
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    adapter_ids: self.options.adapter_ids.clone().unwrap_or_default(),
                    roles: self.options.roles.clone().unwrap_or_default(),
                    native_kinds: self.options.native_kinds.clone().unwrap_or_default(),
                    branch_kind: self.options.branch_kind.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_SEARCH_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TimelineTask {
    type Output = EngineTimelinePage;
    type JsValue = EngineTimelinePage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .timeline_cancellable(
                TimelinePageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    roles: self.options.roles.clone().unwrap_or_default(),
                    native_kinds: self.options.native_kinds.clone().unwrap_or_default(),
                    include_content_kinds: self
                        .options
                        .include_content_kinds
                        .clone()
                        .unwrap_or_default(),
                    include_tool_names: self.options.include_tool_names.clone().unwrap_or_default(),
                    exclude_content_kinds: self
                        .options
                        .exclude_content_kinds
                        .clone()
                        .unwrap_or_default(),
                    exclude_tool_names: self.options.exclude_tool_names.clone().unwrap_or_default(),
                    search: self.options.search.clone(),
                    branch_kind: self.options.branch_kind.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_TIMELINE_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for DelegationsTask {
    type Output = EngineDelegationPage;
    type JsValue = EngineDelegationPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .delegations_cancellable(
                DelegationPageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    workflow_id: self.options.workflow_id.clone(),
                    standalone_only: self.options.standalone_only.unwrap_or(false),
                    cursor: self.options.cursor.clone(),
                    limit: self
                        .options
                        .limit
                        .unwrap_or(DEFAULT_ORCHESTRATION_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for WorkflowsTask {
    type Output = EngineWorkflowPage;
    type JsValue = EngineWorkflowPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .workflows_cancellable(
                WorkflowPageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self
                        .options
                        .limit
                        .unwrap_or(DEFAULT_ORCHESTRATION_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for WorkflowDetailsTask {
    type Output = EngineWorkflowDetails;
    type JsValue = EngineWorkflowDetails;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .workflow_details_cancellable(
                WorkflowDetailsRequest {
                    workflow_id: self.workflow_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for WorkflowMembersTask {
    type Output = EngineWorkflowMemberPage;
    type JsValue = EngineWorkflowMemberPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .workflow_members_cancellable(
                WorkflowMemberPageRequest {
                    workflow_id: self.options.workflow_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self
                        .options
                        .limit
                        .unwrap_or(DEFAULT_ORCHESTRATION_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for MemoryDocumentsTask {
    type Output = EngineMemoryDocumentPage;
    type JsValue = EngineMemoryDocumentPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .memory_documents_cancellable(
                MemoryDocumentPageRequest {
                    project_id: self.options.project_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_CAPABILITY_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TaskCollectionsTask {
    type Output = EngineTaskCollectionPage;
    type JsValue = EngineTaskCollectionPage;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self
            .options
            .clone()
            .unwrap_or(EngineTaskCollectionPageOptions {
                session_id: None,
                run_id: None,
                team_id: None,
                cursor: None,
                limit: None,
            });
        self.engine
            .task_collections_cancellable(
                TaskCollectionPageRequest {
                    session_id: options.session_id,
                    run_id: options.run_id,
                    team_id: options.team_id,
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_CAPABILITY_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TasksTask {
    type Output = EngineTaskPage;
    type JsValue = EngineTaskPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .tasks_cancellable(
                TaskPageRequest {
                    collection_id: self.options.collection_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_CAPABILITY_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for PlansTask {
    type Output = EnginePlanPage;
    type JsValue = EnginePlanPage;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self.options.clone().unwrap_or(EngineCapabilityPageOptions {
            cursor: None,
            limit: None,
        });
        self.engine
            .plans_cancellable(
                PlanPageRequest {
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_CAPABILITY_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for ToolResultsTask {
    type Output = EngineToolResultPage;
    type JsValue = EngineToolResultPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .tool_results_cancellable(
                ToolResultPageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_CAPABILITY_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for ArtifactsTask {
    type Output = EngineArtifactPage;
    type JsValue = EngineArtifactPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .artifacts_cancellable(
                ArtifactPageRequest {
                    session_id: self.options.session_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_CAPABILITY_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for SourcesTask {
    type Output = EngineSourcePage;
    type JsValue = EngineSourcePage;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self.options.clone().unwrap_or(EngineHistoryPageOptions {
            cursor: None,
            limit: None,
        });
        self.engine
            .sources_cancellable(
                SourcePageRequest {
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_DETAIL_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for CanonicalStatsTask {
    type Output = EngineCanonicalStats;
    type JsValue = EngineCanonicalStats;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .canonical_stats_cancellable(self.cancellation.clone())
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for UsageTotalsTask {
    type Output = EngineUsageTotals;
    type JsValue = EngineUsageTotals;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .usage_totals_cancellable(
                UsageScopeRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for UsageActivityTask {
    type Output = EngineUsageActivity;
    type JsValue = EngineUsageActivity;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .usage_activity_cancellable(
                UsageActivityRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    from: self.options.from.clone(),
                    to: self.options.to.clone(),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for RuntimeUsageV2Task {
    type Output = EngineRuntimeUsageV2Page;
    type JsValue = EngineRuntimeUsageV2Page;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .runtime_usage_v2_cancellable(
                RuntimeUsageV2PageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    actor_run_ref: self.options.actor_run_ref.clone(),
                    affiliation_dimension: self.options.affiliation_dimension.clone(),
                    affiliation_target_ref: self.options.affiliation_target_ref.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self
                        .options
                        .limit
                        .unwrap_or(DEFAULT_RUNTIME_USAGE_V2_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for FactFamilyCoverageTask {
    type Output = EngineFactFamilyCoveragePage;
    type JsValue = EngineFactFamilyCoveragePage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .fact_family_coverage_cancellable(
                FactFamilyCoveragePageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    owner_id: self.options.owner_id.clone(),
                    family: self.options.family.clone(),
                    family_version: self.options.family_version,
                    cursor: self.options.cursor.clone(),
                    limit: self
                        .options
                        .limit
                        .unwrap_or(DEFAULT_FACT_FAMILY_COVERAGE_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for RuntimeSnapshotTask {
    type Output = EngineRuntimeSnapshot;
    type JsValue = EngineRuntimeSnapshot;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self
            .options
            .clone()
            .unwrap_or(EngineRuntimeSnapshotOptions {
                project_id: None,
                session_id: None,
                cursor: None,
                limit: None,
            });
        self.engine
            .runtime_snapshot_cancellable(
                RuntimeSnapshotRequest {
                    project_id: options.project_id,
                    session_id: options.session_id,
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_RUNTIME_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for RunStateTask {
    type Output = EngineRunStateLookup;
    type JsValue = EngineRunStateLookup;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .run_state_cancellable(
                RunStateRequest {
                    run_id: self.run_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamsTask {
    type Output = EngineTeamPage;
    type JsValue = EngineTeamPage;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self.options.clone().unwrap_or(EngineTeamPageOptions {
            cursor: None,
            limit: None,
        });
        self.engine
            .teams_cancellable(
                TeamPageRequest {
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_TEAM_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamDetailsTask {
    type Output = EngineTeamDetails;
    type JsValue = EngineTeamDetails;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .team_details_cancellable(
                TeamDetailsRequest {
                    team_id: self.team_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamInboxesTask {
    type Output = EngineTeamInboxPage;
    type JsValue = EngineTeamInboxPage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .team_inboxes_cancellable(
                TeamInboxPageRequest {
                    team_id: self.options.team_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_TEAM_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamInboxMessagesTask {
    type Output = EngineTeamInboxMessagePage;
    type JsValue = EngineTeamInboxMessagePage;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .team_inbox_messages_cancellable(
                TeamInboxMessagePageRequest {
                    inbox_id: self.options.inbox_id.clone(),
                    cursor: self.options.cursor.clone(),
                    limit: self.options.limit.unwrap_or(DEFAULT_TEAM_PAGE_LIMIT),
                },
                self.cancellation.clone(),
            )
            .map(Into::into)
            .map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct DisposeTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl Task for DisposeTask {
    type Output = EngineStatus;
    type JsValue = EngineStatus;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine.shutdown().map_err(napi_error)?;
        Ok(self.engine.status().into())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

fn napi_error(error: EngineError) -> Error {
    let status = match &error {
        EngineError::InvalidConfig(_)
        | EngineError::InvalidQuery(_)
        | EngineError::InvalidCommit(_) => Status::InvalidArg,
        EngineError::QueryCancelled => Status::Cancelled,
        EngineError::QueryQueueFull => Status::QueueFull,
        EngineError::ShuttingDown => Status::Closing,
        _ => Status::GenericFailure,
    };
    Error::new(status, error.to_string())
}

fn cancellation_for_signal(signal: Option<&AbortSignal>) -> QueryCancellationToken {
    let cancellation = QueryCancellationToken::default();
    if let Some(signal) = signal {
        let abort_cancellation = cancellation.clone();
        signal.on_abort(move || abort_cancellation.cancel());
    }
    cancellation
}

fn change_cursor_from_js(value: EngineChangeCursor) -> Result<ChangeCursor> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !value.commit_seq.is_finite()
        || value.commit_seq.fract() != 0.0
        || !(0.0..=MAX_SAFE_INTEGER).contains(&value.commit_seq)
    {
        return Err(Error::new(
            Status::InvalidArg,
            "change cursor commitSeq must be a non-negative safe integer",
        ));
    }
    Ok(ChangeCursor {
        commit_seq: value.commit_seq as u64,
        ordinal: value.ordinal,
    })
}

fn validate_roots(roots: &[String], operation: &str) -> Result<()> {
    if roots.is_empty() || roots.iter().any(|root| root.trim().is_empty()) {
        return Err(Error::new(
            Status::InvalidArg,
            format!("{operation} requires at least one non-empty root"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod support_binding_tests {
    use crate::adapter::{
        verify_support_release_bundle, AgentAdapter, SupportBundleDocument, SupportReleaseStatus,
    };
    use crate::claude::ClaudeCodeAdapter;
    use crate::codex::CodexAdapter;
    use crate::grok::GrokAdapter;

    #[test]
    fn compiled_adapters_match_their_digest_bound_candidate_packages() {
        assert_candidate_binding(
            &ClaudeCodeAdapter::new(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/claude-code/candidate-2026-08-15/support-release.json"
            )),
            &[
                SupportBundleDocument::new(
                    "agent-support/claude-code/candidate-2026-08-15/ads.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/candidate-2026-08-15/ads.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/candidate-2026-08-15/source-declarations.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/candidate-2026-08-15/source-declarations.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/candidate-2026-08-15/scope-programs.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/candidate-2026-08-15/scope-programs.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/candidate-2026-08-15/evidence.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/candidate-2026-08-15/evidence.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/candidate-2026-08-15/conformance.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/candidate-2026-08-15/conformance.json"
                    )),
                ),
            ],
        );
        assert_candidate_binding(
            &CodexAdapter::new(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/codex/candidate-2026-08-15/support-release.json"
            )),
            &[
                SupportBundleDocument::new(
                    "agent-support/codex/candidate-2026-08-15/ads.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/candidate-2026-08-15/ads.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/candidate-2026-08-15/source-declarations.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/candidate-2026-08-15/source-declarations.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/candidate-2026-08-15/scope-programs.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/candidate-2026-08-15/scope-programs.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/candidate-2026-08-15/evidence.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/candidate-2026-08-15/evidence.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/candidate-2026-08-15/conformance.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/candidate-2026-08-15/conformance.json"
                    )),
                ),
            ],
        );
        assert_candidate_binding(
            &GrokAdapter::new(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/grok/candidate-2026-08-15/support-release.json"
            )),
            &[
                SupportBundleDocument::new(
                    "agent-support/grok/candidate-2026-08-15/ads.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/candidate-2026-08-15/ads.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/candidate-2026-08-15/source-declarations.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/candidate-2026-08-15/source-declarations.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/candidate-2026-08-15/scope-programs.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/candidate-2026-08-15/evidence.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/candidate-2026-08-15/evidence.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/candidate-2026-08-15/conformance.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/candidate-2026-08-15/conformance.json"
                    )),
                ),
            ],
        );
    }

    fn assert_candidate_binding(
        adapter: &dyn AgentAdapter,
        release_json: &[u8],
        documents: &[SupportBundleDocument<'_>],
    ) {
        let release = verify_support_release_bundle(release_json, documents).unwrap();
        let manifest = adapter.manifest();
        release
            .verify_adapter_binding(
                manifest.id.as_str(),
                manifest
                    .support_binding
                    .as_ref()
                    .expect("built-in adapter must declare its support binding"),
            )
            .unwrap();
        release
            .verify_scope_programs(
                manifest
                    .scope_programs
                    .as_ref()
                    .expect("built-in adapter must compile its scope declaration"),
            )
            .unwrap();
        assert_eq!(release.descriptor().status, SupportReleaseStatus::Candidate);
    }
}
