//! N-API host adapter for the library-first persistent engine.

use std::path::PathBuf;
use std::sync::Arc;

use napi::bindgen_prelude::{AbortSignal, AsyncTask, Env, Error, Result, Status, Task};
use napi_derive::napi;

use crate::engine::{
    CanonicalStats, EngineHealthSnapshot, EngineOptions, EngineOverview, EngineStatusSnapshot,
    HistoryProjectIndexSummary, HistoryProjectPage, HistoryProjectPageRequest,
    HistoryProjectSummary, HistorySessionIndexSummary, HistorySessionPage,
    HistorySessionPageRequest, HistorySessionSummary, MessageDetail, MessagePage,
    MessagePageRequest, NamedCount, ObservationStatusSnapshot, ObservationSupervisorOptions,
    OwnerMetadata, QueryCancellationToken, ReconcileOutcome, ReconcileRequest, RunStateLookup,
    RunStateRequest, RuntimePresenceSnapshot, RuntimeRunEvidence, RuntimeRunSnapshot,
    RuntimeSnapshot, RuntimeSnapshotRequest, SessionDetail, SessionDetails, SessionDetailsRequest,
    SessionIndexDetail, SourcePage, SourcePageRequest, SourceSummary, SpaghettiEngineCore,
    TeamConfigSummary, TeamDetails, TeamDetailsRequest, TeamInboxMessage, TeamInboxMessagePage,
    TeamInboxMessagePageRequest, TeamInboxPage, TeamInboxPageRequest, TeamInboxSummary, TeamMember,
    TeamPage, TeamPageRequest, TeamSummary, UntimedUsageSummary, UsageActivityDay,
    UsageActivityReport, UsageActivityRequest, UsageAggregate, UsageCoverageSummary,
    UsageScopeRequest, UsageTokenValues, UsageTotalsReport, DEFAULT_DETAIL_PAGE_LIMIT,
    DEFAULT_HISTORY_PAGE_LIMIT, DEFAULT_RUNTIME_PAGE_LIMIT, DEFAULT_TEAM_PAGE_LIMIT,
};

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineOpenOptions {
    /// Canonical SQLite database owned by this engine instance.
    pub db_path: String,
    /// Number of persistent read-only workers. Defaults to 2; maximum 16.
    pub query_workers: Option<u32>,
    /// Diagnostic host label persisted in the owner metadata sidecar.
    pub owner_label: Option<String>,
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
    pub writer_data_version: u32,
    pub journal_mode: String,
    pub query_only: bool,
    pub read_only: bool,
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
pub struct EngineSourceSummary {
    pub source_id: String,
    pub source_instance_id: f64,
    pub adapter_id: String,
    pub display_name: String,
    pub adapter_contract_version: u32,
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
            adapter_contract_version: value.adapter_contract_version,
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
pub struct EngineObservationOptions {
    /// Configured native data roots understood by the selected adapter.
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

    /// Reconcile the adapter-declared Claude source map through the common
    /// Rust drivers, decoders, projections, and durable cursor transaction.
    #[napi(ts_return_type = "Promise<EngineReconcileResult>")]
    pub fn reconcile_claude(
        &self,
        options: EngineReconcileOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<ReconcileClaudeTask> {
        AsyncTask::with_optional_signal(
            ReconcileClaudeTask {
                engine: Arc::clone(&self.inner),
                options,
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
        AsyncTask::with_optional_signal(
            StartClaudeObservationTask {
                engine: Arc::clone(&self.inner),
                options,
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
        AsyncTask::with_optional_signal(
            RefreshClaudeObservationTask {
                engine: Arc::clone(&self.inner),
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

impl Task for OpenEngineTask {
    type Output = SpaghettiEngine;
    type JsValue = SpaghettiEngine;

    fn compute(&mut self) -> Result<Self::Output> {
        let query_workers = self
            .options
            .query_workers
            .map(|value| usize::try_from(value).unwrap_or(usize::MAX));
        let inner = SpaghettiEngineCore::open(EngineOptions {
            database_path: PathBuf::from(&self.options.db_path),
            query_workers,
            owner_label: self.options.owner_label.clone(),
        })
        .map_err(napi_error)?;
        Ok(SpaghettiEngine { inner })
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
    options: EngineReconcileOptions,
}

pub struct StartClaudeObservationTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineObservationOptions,
}

pub struct RefreshClaudeObservationTask {
    engine: Arc<SpaghettiEngineCore>,
}

pub struct StopClaudeObservationTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl Task for ReconcileClaudeTask {
    type Output = EngineReconcileResult;
    type JsValue = EngineReconcileResult;

    fn compute(&mut self) -> Result<Self::Output> {
        validate_roots(&self.options.roots, "reconcileClaude")?;
        let request = ReconcileRequest {
            configured_roots: self.options.roots.iter().map(PathBuf::from).collect(),
            reason: self
                .options
                .reason
                .clone()
                .unwrap_or_else(|| "manual_reconcile".to_string()),
        };
        self.engine
            .reconcile_claude(request)
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
        validate_roots(&self.options.roots, "startClaudeObservation")?;
        let mut options = ObservationSupervisorOptions::new(
            self.options.roots.iter().map(PathBuf::from).collect(),
        );
        if let Some(reason) = self.options.reason.clone() {
            options.reason = reason;
        }
        self.engine
            .start_claude_observation(options)
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
            .refresh_claude_observation()
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
        self.engine.stop_claude_observation().map_err(napi_error)?;
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

fn napi_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

fn cancellation_for_signal(signal: Option<&AbortSignal>) -> QueryCancellationToken {
    let cancellation = QueryCancellationToken::default();
    if let Some(signal) = signal {
        let abort_cancellation = cancellation.clone();
        signal.on_abort(move || abort_cancellation.cancel());
    }
    cancellation
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
