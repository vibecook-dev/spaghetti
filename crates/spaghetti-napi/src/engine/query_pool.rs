//! Bounded pool of persistent, read-only SQLite query workers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::core::schema;
use crate::source::SharedSourcePassPool;

use super::capability_query::{
    read_artifact_page, read_memory_document_page, read_plan_page, read_task_collection_page,
    read_task_page, read_tool_result_page, validate_artifact_page, validate_memory_document_page,
    validate_plan_page, validate_task_collection_page, validate_task_page,
    validate_tool_result_page, ArtifactPage, ArtifactPageRequest, MemoryDocumentPage,
    MemoryDocumentPageRequest, PlanPage, PlanPageRequest, TaskCollectionPage,
    TaskCollectionPageRequest, TaskPage, TaskPageRequest, ToolResultPage, ToolResultPageRequest,
};
use super::catalog::{
    encode_external_ref, read_project_page, read_readiness, read_session_page,
    resolve_catalog_entity, CatalogEntityResolution, CatalogProjectPage, CatalogProjectPageRequest,
    CatalogSessionPage, CatalogSessionPageRequest, Readiness, HISTORY_PROJECT_CATALOG_COLUMNS,
    HISTORY_PROJECT_CATALOG_CTE, HISTORY_PROJECT_CATALOG_JOINS, HISTORY_SESSION_CATALOG_COLUMNS,
    HISTORY_SESSION_CATALOG_JOIN,
};
use super::coverage_query::{
    read_fact_family_coverage_page, read_fact_family_replay_target,
    validate_fact_family_coverage_page, validate_fact_family_replay_target, FactFamilyCoveragePage,
    FactFamilyCoveragePageRequest, FactFamilyReplayTarget, FactFamilyReplayTargetRequest,
};
use super::detail_query::{
    read_canonical_stats, read_message_page, read_session_details, read_source_page,
    validate_message_page, validate_session_details, validate_source_page, CanonicalStats,
    MessagePage, MessagePageRequest, SessionDetails, SessionDetailsRequest, SourcePage,
    SourcePageRequest,
};
use super::orchestration_query::{
    read_delegation_page, read_workflow_details, read_workflow_member_page, read_workflow_page,
    validate_delegation_page, validate_workflow_details, validate_workflow_member_page,
    validate_workflow_page, DelegationPage, DelegationPageRequest, WorkflowDetails,
    WorkflowDetailsRequest, WorkflowMemberPage, WorkflowMemberPageRequest, WorkflowPage,
    WorkflowPageRequest,
};
use super::performance::{
    atomic_max, atomic_saturating_add, duration_ns, LatencyHistogram, NamedLatencySnapshot,
    QueryPerformanceSnapshot, RuntimeUsageCompatibilityTelemetrySnapshot,
};
use super::query_identity::{
    decode_entity_id, encode_entity_id, PROJECT_ID_PREFIX, SESSION_ID_PREFIX,
};
use super::runtime_query::{
    read_run_state, read_runtime_snapshot, validate_run_state_request, validate_runtime_request,
    RunStateLookup, RunStateRequest, RuntimeSnapshot, RuntimeSnapshotRequest,
};
use super::runtime_usage_query::{
    read_runtime_usage_query_selection_target, read_runtime_usage_v2_page,
    validate_runtime_usage_query_selection_target, validate_runtime_usage_v2_page,
    RuntimeUsageQuerySelectionTarget, RuntimeUsageQuerySelectionTargetRequest, RuntimeUsageV2Page,
    RuntimeUsageV2PageRequest,
};
use super::runtime_usage_totals_query::{
    read_runtime_usage_compatibility, read_runtime_usage_totals,
    validate_runtime_usage_compatibility, validate_runtime_usage_totals,
    RuntimeUsageCompatibilityReport, RuntimeUsageCompatibilityRequest, RuntimeUsageTotalsReport,
    RuntimeUsageTotalsRequest,
};
use super::search_query::{read_search_page, validate_search_page, SearchPage, SearchPageRequest};
use super::team_query::{
    read_team_details, read_team_inbox_page, read_team_message_page, read_team_page,
    validate_team_details, validate_team_inbox_page, validate_team_message_page,
    validate_team_page, TeamDetails, TeamDetailsRequest, TeamInboxMessagePage,
    TeamInboxMessagePageRequest, TeamInboxPage, TeamInboxPageRequest, TeamPage, TeamPageRequest,
};
use super::timeline_query::{
    read_timeline_page, validate_timeline_page, TimelinePage, TimelinePageRequest,
};
use super::usage_query::{
    read_usage_activity, read_usage_totals, validate_usage_activity, validate_usage_scope,
    UsageActivityReport, UsageActivityRequest, UsageScopeRequest, UsageTotalsReport,
};
use super::EngineError;

const QUEUE_DEPTH_PER_WORKER: usize = 16;
pub const HISTORY_QUERY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_HISTORY_PAGE_LIMIT: u32 = 50;
const MAX_HISTORY_PAGE_LIMIT: u32 = 200;
const MAX_HISTORY_TOKEN_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOverview {
    pub schema_version: u32,
    pub commit_seq: u64,
    /// Transitional compatibility-table counts. New RFC 011 observation
    /// commits intentionally do not populate these tables.
    pub projects: u32,
    pub sessions: u32,
    pub messages: u32,
    /// Canonical history materialized by the common observation coordinator.
    pub canonical_sessions: u32,
    pub canonical_messages: u32,
    /// Durable replay-retention metrics from the same read snapshot.
    pub change_log_oldest_cursor: Option<ChangeCursor>,
    pub change_log_pruned_through_seq: u64,
    pub change_log_retained_changes: u64,
    pub change_log_retained_payload_bytes: u64,
    pub query_only: bool,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryProjectPageRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryProjectPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub items: Vec<HistoryProjectSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryProjectSummary {
    pub project_id: String,
    pub adapter_id: String,
    pub source_instance_id: u64,
    pub native_project_key: String,
    /// Transcript-backed canonical sessions. Index-only entries are reported
    /// separately so native metadata never becomes fabricated history.
    pub transcript_session_count: u64,
    pub message_count: u64,
    pub memory_document_count: u64,
    /// True when a canonical native memory-index document (for Claude,
    /// `MEMORY.md`) exists. Topic documents alone do not set this flag.
    pub has_memory_index: bool,
    pub latest_activity_at: Option<String>,
    pub latest_activity_source: Option<String>,
    pub index: Option<HistoryProjectIndexSummary>,
    /// Catalog facts for this row; see `engine::catalog`. Absent until
    /// discovery has run for this row's source.
    pub external_ref: Option<String>,
    pub catalog_state: Option<String>,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryProjectIndexSummary {
    pub status: String,
    pub original_path: Option<String>,
    pub entry_count: u64,
    pub assertion_count: u64,
    pub competing_snapshot_count: u64,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySessionPageRequest {
    pub project_id: String,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySessionPage {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub items: Vec<HistorySessionSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySessionSummary {
    pub session_id: String,
    pub project_id: String,
    pub native_session_id: String,
    pub native_project_key: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
    pub message_count: u64,
    pub first_message_at: Option<String>,
    pub first_message_time_quality: Option<String>,
    pub last_message_at: Option<String>,
    pub last_message_time_quality: Option<String>,
    pub latest_activity_at: Option<String>,
    pub latest_activity_source: Option<String>,
    pub index: Option<HistorySessionIndexSummary>,
    /// Catalog facts for this row; see `engine::catalog`. Absent until
    /// discovery has run for this row's source.
    pub external_ref: Option<String>,
    pub catalog_state: Option<String>,
    pub last_commit_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySessionIndexSummary {
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

/// Snapshot-consistent durable source state used by the common observation
/// coordinator to resume drivers after restart. This is not a public semantic
/// query: it exposes only common catalog state inside the Rust engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCatalogSnapshot {
    pub source_instance_id: Option<u64>,
    pub adapter_contract_version: Option<u32>,
    pub streams: Vec<SourceCatalogStream>,
    pub objects: Vec<SourceCatalogObject>,
    pub projection_versions: Vec<SourceCatalogProjectionVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCatalogStream {
    pub source_stream_id: u64,
    pub stream_key: String,
    pub driver_kind: String,
    pub decoder_key: String,
    pub stream_state: String,
    pub last_reconciled_at: Option<i64>,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCatalogObject {
    pub source_stream_id: u64,
    pub source_object_id: u64,
    pub stream_key: String,
    pub object_key: Vec<u8>,
    pub display_path: Option<String>,
    pub native_identity: Option<Vec<u8>>,
    pub generation: u64,
    pub committed_cursor: Vec<u8>,
    pub observed_revision: Option<Vec<u8>>,
    pub adapter_object_context: Option<Vec<u8>>,
    pub driver_checkpoint: Option<Vec<u8>>,
    pub driver_checkpoint_version: Option<u32>,
    pub decoder_state: Option<Vec<u8>>,
    pub decoder_state_version: Option<u32>,
    pub retry_state: Option<Vec<u8>>,
    pub size_bytes: Option<u64>,
    pub mtime_ns: Option<i64>,
    pub decoder_contract_version: u32,
    pub last_commit_seq: Option<u64>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCatalogProjectionVersion {
    pub projection_id: String,
    pub desired_version: u32,
    pub completed_version: Option<u32>,
    pub readiness: String,
    pub last_commit_seq: Option<u64>,
    pub updated_at: i64,
    pub detail: Option<String>,
}

/// Internal, bounded generation baseline used to resume an explicit
/// fact-family replay after a process restart. The normalized coverage set is
/// the durable baseline; this query deliberately exposes no native paths or
/// adapter payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceCoverageReplayBaseline {
    pub source_instance_id: u64,
    pub adapter_id: String,
    pub canonical_source_instance_key: Vec<u8>,
    pub completeness: String,
    pub content_digest: Vec<u8>,
    pub last_commit_seq: u64,
    pub members: Vec<SourceCoverageReplayMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SourceCoverageReplayMember {
    pub stream_key: Vec<u8>,
    pub object_key: Vec<u8>,
    pub generation: u64,
    pub absent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangeCursor {
    pub commit_seq: u64,
    pub ordinal: u32,
}

impl ChangeCursor {
    /// A cursor positioned after every change in a committed query snapshot.
    pub fn after_snapshot(commit_seq: u64) -> Self {
        Self {
            commit_seq,
            ordinal: u32::MAX,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeReplayRequest {
    pub after: Option<ChangeCursor>,
    /// Empty means all stable topics.
    pub topics: Vec<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableChange {
    pub cursor: ChangeCursor,
    pub topic: String,
    pub schema_version: u32,
    pub entity_key: Vec<u8>,
    pub operation: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeReplay {
    pub contract_version: u32,
    /// Watermark read at the start of the same SQLite snapshot as `changes`.
    pub at_commit_seq: u64,
    pub oldest_available: Option<ChangeCursor>,
    pub changes: Vec<DurableChange>,
    pub next_cursor: Option<ChangeCursor>,
    pub has_more: bool,
    pub payload_bytes: u64,
    pub payload_byte_limit: u64,
}

pub const CHANGE_REPLAY_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_CHANGE_REPLAY_LIMIT: u32 = 100;
pub const MAX_CHANGE_REPLAY_PAYLOAD_BYTES: u64 = 12 * 1024 * 1024;

enum QueryCommand {
    Overview {
        cancellation_epoch: u64,
        response: Sender<Result<QueryOverview, EngineError>>,
    },
    HistoryProjects {
        cancellation_epoch: u64,
        request: HistoryProjectPageRequest,
        response: Sender<Result<HistoryProjectPage, EngineError>>,
    },
    HistorySessions {
        cancellation_epoch: u64,
        request: HistorySessionPageRequest,
        response: Sender<Result<HistorySessionPage, EngineError>>,
    },
    SessionDetails {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: SessionDetailsRequest,
        response: Sender<Result<SessionDetails, EngineError>>,
    },
    Messages {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: MessagePageRequest,
        response: Sender<Result<MessagePage, EngineError>>,
    },
    Search {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: SearchPageRequest,
        response: Sender<Result<SearchPage, EngineError>>,
    },
    Timeline {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: TimelinePageRequest,
        response: Sender<Result<TimelinePage, EngineError>>,
    },
    Delegations {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: DelegationPageRequest,
        response: Sender<Result<DelegationPage, EngineError>>,
    },
    Workflows {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: WorkflowPageRequest,
        response: Sender<Result<WorkflowPage, EngineError>>,
    },
    WorkflowDetails {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: WorkflowDetailsRequest,
        response: Sender<Result<WorkflowDetails, EngineError>>,
    },
    WorkflowMembers {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: WorkflowMemberPageRequest,
        response: Sender<Result<WorkflowMemberPage, EngineError>>,
    },
    MemoryDocuments {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: MemoryDocumentPageRequest,
        response: Sender<Result<MemoryDocumentPage, EngineError>>,
    },
    TaskCollections {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: TaskCollectionPageRequest,
        response: Sender<Result<TaskCollectionPage, EngineError>>,
    },
    Tasks {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: TaskPageRequest,
        response: Sender<Result<TaskPage, EngineError>>,
    },
    Plans {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: PlanPageRequest,
        response: Sender<Result<PlanPage, EngineError>>,
    },
    ToolResults {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: ToolResultPageRequest,
        response: Sender<Result<ToolResultPage, EngineError>>,
    },
    Artifacts {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: ArtifactPageRequest,
        response: Sender<Result<ArtifactPage, EngineError>>,
    },
    UsageTotals {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: UsageScopeRequest,
        response: Sender<Result<UsageTotalsReport, EngineError>>,
    },
    UsageActivity {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: UsageActivityRequest,
        response: Sender<Result<UsageActivityReport, EngineError>>,
    },
    RuntimeUsageV2 {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: RuntimeUsageV2PageRequest,
        response: Sender<Result<RuntimeUsageV2Page, EngineError>>,
    },
    RuntimeUsageTotals {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: RuntimeUsageTotalsRequest,
        response: Sender<Result<RuntimeUsageTotalsReport, EngineError>>,
    },
    RuntimeUsageCompatibility {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: RuntimeUsageCompatibilityRequest,
        response: Sender<Result<RuntimeUsageCompatibilityReport, EngineError>>,
    },
    RuntimeUsageQuerySelectionTarget {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: RuntimeUsageQuerySelectionTargetRequest,
        response: Sender<Result<RuntimeUsageQuerySelectionTarget, EngineError>>,
    },
    FactFamilyCoverage {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: FactFamilyCoveragePageRequest,
        response: Sender<Result<FactFamilyCoveragePage, EngineError>>,
    },
    FactFamilyReplayTarget {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: FactFamilyReplayTargetRequest,
        response: Sender<Result<FactFamilyReplayTarget, EngineError>>,
    },
    RuntimeSnapshot {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: RuntimeSnapshotRequest,
        response: Sender<Result<RuntimeSnapshot, EngineError>>,
    },
    RunState {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: RunStateRequest,
        response: Sender<Result<RunStateLookup, EngineError>>,
    },
    Sources {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: SourcePageRequest,
        response: Sender<Result<SourcePage, EngineError>>,
    },
    CanonicalStats {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        response: Sender<Result<CanonicalStats, EngineError>>,
    },
    Teams {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: TeamPageRequest,
        response: Sender<Result<TeamPage, EngineError>>,
    },
    TeamDetails {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: TeamDetailsRequest,
        response: Sender<Result<TeamDetails, EngineError>>,
    },
    TeamInboxes {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: TeamInboxPageRequest,
        response: Sender<Result<TeamInboxPage, EngineError>>,
    },
    TeamInboxMessages {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: TeamInboxMessagePageRequest,
        response: Sender<Result<TeamInboxMessagePage, EngineError>>,
    },
    ReplayChanges {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: ChangeReplayRequest,
        response: Sender<Result<ChangeReplay, EngineError>>,
    },
    CatalogProjects {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: Box<CatalogProjectPageRequest>,
        response: Sender<Result<CatalogProjectPage, EngineError>>,
    },
    CatalogSessions {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        request: Box<CatalogSessionPageRequest>,
        response: Sender<Result<CatalogSessionPage, EngineError>>,
    },
    CatalogResolve {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        external_ref: String,
        response: Sender<Result<CatalogEntityResolution, EngineError>>,
    },
    CatalogReadiness {
        cancellation_epoch: u64,
        cancellation: QueryCancellationToken,
        response: Sender<Result<Readiness, EngineError>>,
    },
    SourceCatalog {
        cancellation_epoch: u64,
        adapter_id: String,
        stable_key: Vec<u8>,
        response: Sender<Result<SourceCatalogSnapshot, EngineError>>,
    },
    SourceCoverageReplayBaseline {
        cancellation_epoch: u64,
        source_instance_id: u64,
        owner_id: String,
        owner_scope_key: Vec<u8>,
        family: String,
        version: u32,
        response: Sender<Result<Option<SourceCoverageReplayBaseline>, EngineError>>,
    },
    #[cfg(test)]
    Hold {
        entered: Sender<()>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    HoldReadSnapshot {
        entered: Sender<Result<(), EngineError>>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    ProbeWrite(Sender<bool>),
    Shutdown,
}

struct QueuedQuery {
    command: QueryCommand,
    queued_at: Instant,
    measured: bool,
    _source_pass: Option<crate::source::SharedSourcePassPermit>,
}

impl QueuedQuery {
    fn measured(
        command: QueryCommand,
        queued_at: Instant,
        source_pass: Option<crate::source::SharedSourcePassPermit>,
    ) -> Self {
        Self {
            command,
            queued_at,
            measured: true,
            _source_pass: source_pass,
        }
    }

    fn control(command: QueryCommand) -> Self {
        Self {
            command,
            queued_at: Instant::now(),
            measured: false,
            _source_pass: None,
        }
    }
}

struct QueryControl {
    cancellation_epoch: AtomicU64,
    stopping: AtomicBool,
    alive_workers: AtomicUsize,
    in_flight: AtomicUsize,
    telemetry: QueryTelemetry,
}

struct QueryTelemetry {
    opened_at: Instant,
    requests_enqueued: AtomicU64,
    queue_rejections: AtomicU64,
    queue_high_watermark: AtomicU64,
    queue_wait: LatencyHistogram,
    execution: LatencyHistogram,
    active_started_ns: Box<[AtomicU64]>,
    runtime_usage_compatibility: RuntimeUsageCompatibilityTelemetry,
}

struct RuntimeUsageCompatibilityTelemetry {
    samples: AtomicU64,
    ready_samples: AtomicU64,
    not_ready_samples: AtomicU64,
    equal_samples: AtomicU64,
    different_samples: AtomicU64,
    incomparable_samples: AtomicU64,
    equal_buckets: AtomicU64,
    legacy_higher_buckets: AtomicU64,
    v2_higher_buckets: AtomicU64,
    incomparable_buckets: AtomicU64,
    sampled_absolute_delta_tokens: AtomicU64,
    max_absolute_delta_tokens: AtomicU64,
    first_at_commit_seq: AtomicU64,
    last_at_commit_seq: AtomicU64,
}

/// Transport-neutral, request-scoped cancellation observed by query workers.
#[derive(Clone)]
pub struct QueryCancellationToken {
    cancelled: Arc<AtomicBool>,
    changed: tokio::sync::watch::Sender<bool>,
}

impl Default for QueryCancellationToken {
    fn default() -> Self {
        let (changed, _) = tokio::sync::watch::channel(false);
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            changed,
        }
    }
}

impl QueryCancellationToken {
    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.changed.send_replace(true);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut changed = self.changed.subscribe();
        if *changed.borrow_and_update() {
            return;
        }
        while changed.changed().await.is_ok() {
            if *changed.borrow_and_update() {
                return;
            }
        }
    }
}

impl QueryControl {
    fn new(workers: usize) -> Self {
        Self {
            cancellation_epoch: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
            alive_workers: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            telemetry: QueryTelemetry {
                opened_at: Instant::now(),
                requests_enqueued: AtomicU64::new(0),
                queue_rejections: AtomicU64::new(0),
                queue_high_watermark: AtomicU64::new(0),
                queue_wait: LatencyHistogram::default(),
                execution: LatencyHistogram::default(),
                active_started_ns: (0..workers).map(|_| AtomicU64::new(0)).collect(),
                runtime_usage_compatibility: RuntimeUsageCompatibilityTelemetry::new(),
            },
        }
    }
}

impl QueryTelemetry {
    fn snapshot(&self, queue_depth: usize) -> QueryPerformanceSnapshot {
        let now_ns = duration_ns(self.opened_at.elapsed());
        let queue_wait = self.queue_wait.snapshot();
        let execution = self.execution.snapshot();
        let oldest_started_ns = self
            .active_started_ns
            .iter()
            .map(|started| started.load(Ordering::Acquire))
            .filter(|started| *started > 0)
            .min();
        QueryPerformanceSnapshot {
            uptime_ns: now_ns,
            requests_enqueued: self.requests_enqueued.load(Ordering::Acquire),
            // The execution histogram publishes its sample count last, after
            // the duration and bucket updates. Use that single publication as
            // the completion counter instead of racing a second atomic that
            // can make one snapshot internally contradictory.
            requests_completed: execution.samples,
            queue_rejections: self.queue_rejections.load(Ordering::Acquire),
            queue_depth: u64::try_from(queue_depth).unwrap_or(u64::MAX),
            queue_high_watermark: self.queue_high_watermark.load(Ordering::Acquire),
            oldest_active_ns: oldest_started_ns
                .map(|started| now_ns.saturating_sub(started))
                .unwrap_or(0),
            runtime_usage_compatibility: self.runtime_usage_compatibility.snapshot(),
            timings: vec![
                NamedLatencySnapshot {
                    name: "queue_wait".to_string(),
                    latency: queue_wait,
                },
                NamedLatencySnapshot {
                    name: "execution".to_string(),
                    latency: execution,
                },
            ],
        }
    }
}

impl RuntimeUsageCompatibilityTelemetry {
    fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            ready_samples: AtomicU64::new(0),
            not_ready_samples: AtomicU64::new(0),
            equal_samples: AtomicU64::new(0),
            different_samples: AtomicU64::new(0),
            incomparable_samples: AtomicU64::new(0),
            equal_buckets: AtomicU64::new(0),
            legacy_higher_buckets: AtomicU64::new(0),
            v2_higher_buckets: AtomicU64::new(0),
            incomparable_buckets: AtomicU64::new(0),
            sampled_absolute_delta_tokens: AtomicU64::new(0),
            max_absolute_delta_tokens: AtomicU64::new(0),
            first_at_commit_seq: AtomicU64::new(u64::MAX),
            last_at_commit_seq: AtomicU64::new(0),
        }
    }

    fn record(&self, report: &RuntimeUsageCompatibilityReport) {
        let _ = self
            .first_at_commit_seq
            .fetch_min(report.at_commit_seq, Ordering::AcqRel);
        atomic_max(&self.last_at_commit_seq, report.at_commit_seq);
        match report.status.as_str() {
            "ready" => atomic_saturating_add(&self.ready_samples, 1),
            "not_ready" => atomic_saturating_add(&self.not_ready_samples, 1),
            _ => {}
        }
        match report.comparison_status.as_str() {
            "equal" => atomic_saturating_add(&self.equal_samples, 1),
            "different" => atomic_saturating_add(&self.different_samples, 1),
            "incomparable" => atomic_saturating_add(&self.incomparable_samples, 1),
            "not_ready" => {}
            _ => {}
        }
        for bucket in [
            report.input_tokens.as_ref(),
            report.output_tokens.as_ref(),
            report.cache_creation_input_tokens.as_ref(),
            report.cache_read_input_tokens.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            match bucket.relation.as_str() {
                "equal" => atomic_saturating_add(&self.equal_buckets, 1),
                "legacy_higher" => atomic_saturating_add(&self.legacy_higher_buckets, 1),
                "v2_higher" => atomic_saturating_add(&self.v2_higher_buckets, 1),
                "incomparable" => atomic_saturating_add(&self.incomparable_buckets, 1),
                _ => continue,
            }
            if let Some(delta) = bucket.absolute_delta_tokens {
                atomic_saturating_add(&self.sampled_absolute_delta_tokens, delta);
                atomic_max(&self.max_absolute_delta_tokens, delta);
            }
        }
        // Publish the sample count last. A snapshot that observes a non-zero
        // count through its acquire load can then also observe the initialized
        // commit bounds and classifications for that sample.
        atomic_saturating_add(&self.samples, 1);
    }

    fn snapshot(&self) -> RuntimeUsageCompatibilityTelemetrySnapshot {
        let samples = self.samples.load(Ordering::Acquire);
        RuntimeUsageCompatibilityTelemetrySnapshot {
            samples,
            ready_samples: self.ready_samples.load(Ordering::Acquire),
            not_ready_samples: self.not_ready_samples.load(Ordering::Acquire),
            equal_samples: self.equal_samples.load(Ordering::Acquire),
            different_samples: self.different_samples.load(Ordering::Acquire),
            incomparable_samples: self.incomparable_samples.load(Ordering::Acquire),
            equal_buckets: self.equal_buckets.load(Ordering::Acquire),
            legacy_higher_buckets: self.legacy_higher_buckets.load(Ordering::Acquire),
            v2_higher_buckets: self.v2_higher_buckets.load(Ordering::Acquire),
            incomparable_buckets: self.incomparable_buckets.load(Ordering::Acquire),
            sampled_absolute_delta_tokens: self
                .sampled_absolute_delta_tokens
                .load(Ordering::Acquire),
            max_absolute_delta_tokens: self.max_absolute_delta_tokens.load(Ordering::Acquire),
            first_at_commit_seq: (samples > 0)
                .then(|| self.first_at_commit_seq.load(Ordering::Acquire)),
            last_at_commit_seq: (samples > 0)
                .then(|| self.last_at_commit_seq.load(Ordering::Acquire)),
        }
    }
}

struct InFlightGuard<'a>(&'a AtomicUsize);

impl<'a> InFlightGuard<'a> {
    fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct QueryMeasurementGuard<'a> {
    telemetry: &'a QueryTelemetry,
    worker_id: usize,
    started_at: Instant,
}

impl<'a> QueryMeasurementGuard<'a> {
    fn begin(telemetry: &'a QueryTelemetry, worker_id: usize, queued_at: Instant) -> Self {
        telemetry.queue_wait.record(queued_at.elapsed());
        let active_since = duration_ns(telemetry.opened_at.elapsed()).max(1);
        telemetry.active_started_ns[worker_id].store(active_since, Ordering::Release);
        Self {
            telemetry,
            worker_id,
            started_at: Instant::now(),
        }
    }
}

impl Drop for QueryMeasurementGuard<'_> {
    fn drop(&mut self) {
        self.telemetry.execution.record(self.started_at.elapsed());
        self.telemetry.active_started_ns[self.worker_id].store(0, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct QueryClient {
    commands: Sender<QueuedQuery>,
    control: Arc<QueryControl>,
    configured_workers: usize,
    source_pass_pool: Option<SharedSourcePassPool>,
}

impl QueryClient {
    pub fn overview(&self) -> Result<QueryOverview, EngineError> {
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(EngineError::ShuttingDown);
        }

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        self.enqueue(QueryCommand::Overview {
            cancellation_epoch,
            response: response_tx,
        })?;

        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    pub fn history_projects(
        &self,
        request: HistoryProjectPageRequest,
    ) -> Result<HistoryProjectPage, EngineError> {
        validate_history_page_limit(request.limit)?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| decode_history_cursor(cursor, HistoryCursorKind::Projects, None))
            .transpose()?;
        self.ensure_running()?;

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        self.enqueue(QueryCommand::HistoryProjects {
            cancellation_epoch,
            request: HistoryProjectPageRequest {
                cursor: cursor
                    .map(|cursor| encode_history_cursor(&cursor))
                    .transpose()?,
                limit: request.limit,
            },
            response: response_tx,
        })?;

        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    pub fn history_sessions(
        &self,
        request: HistorySessionPageRequest,
    ) -> Result<HistorySessionPage, EngineError> {
        validate_history_page_limit(request.limit)?;
        let project_key = decode_entity_id(&request.project_id, PROJECT_ID_PREFIX, "project id")?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| {
                decode_history_cursor(
                    cursor,
                    HistoryCursorKind::Sessions,
                    Some(request.project_id.as_str()),
                )
            })
            .transpose()?;
        self.ensure_running()?;

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        self.enqueue(QueryCommand::HistorySessions {
            cancellation_epoch,
            request: HistorySessionPageRequest {
                project_id: encode_entity_id(PROJECT_ID_PREFIX, &project_key),
                cursor: cursor
                    .map(|cursor| encode_history_cursor(&cursor))
                    .transpose()?,
                limit: request.limit,
            },
            response: response_tx,
        })?;

        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    pub fn usage_totals(
        &self,
        request: UsageScopeRequest,
    ) -> Result<UsageTotalsReport, EngineError> {
        self.usage_totals_cancellable(request, QueryCancellationToken::default())
    }

    pub fn session_details(
        &self,
        request: SessionDetailsRequest,
    ) -> Result<SessionDetails, EngineError> {
        self.session_details_cancellable(request, QueryCancellationToken::default())
    }

    pub fn session_details_cancellable(
        &self,
        request: SessionDetailsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<SessionDetails, EngineError> {
        validate_session_details(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::SessionDetails {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn messages(&self, request: MessagePageRequest) -> Result<MessagePage, EngineError> {
        self.messages_cancellable(request, QueryCancellationToken::default())
    }

    pub fn messages_cancellable(
        &self,
        request: MessagePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<MessagePage, EngineError> {
        validate_message_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Messages {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn search_cancellable(
        &self,
        request: SearchPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<SearchPage, EngineError> {
        validate_search_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Search {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn timeline_cancellable(
        &self,
        request: TimelinePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TimelinePage, EngineError> {
        validate_timeline_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Timeline {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn delegations_cancellable(
        &self,
        request: DelegationPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<DelegationPage, EngineError> {
        validate_delegation_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Delegations {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn workflows_cancellable(
        &self,
        request: WorkflowPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<WorkflowPage, EngineError> {
        validate_workflow_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Workflows {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn workflow_details_cancellable(
        &self,
        request: WorkflowDetailsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<WorkflowDetails, EngineError> {
        validate_workflow_details(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::WorkflowDetails {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn workflow_members_cancellable(
        &self,
        request: WorkflowMemberPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<WorkflowMemberPage, EngineError> {
        validate_workflow_member_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::WorkflowMembers {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn memory_documents_cancellable(
        &self,
        request: MemoryDocumentPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<MemoryDocumentPage, EngineError> {
        validate_memory_document_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::MemoryDocuments {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn task_collections_cancellable(
        &self,
        request: TaskCollectionPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TaskCollectionPage, EngineError> {
        validate_task_collection_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::TaskCollections {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn tasks_cancellable(
        &self,
        request: TaskPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TaskPage, EngineError> {
        validate_task_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Tasks {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn plans_cancellable(
        &self,
        request: PlanPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<PlanPage, EngineError> {
        validate_plan_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Plans {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn tool_results_cancellable(
        &self,
        request: ToolResultPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<ToolResultPage, EngineError> {
        validate_tool_result_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::ToolResults {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn artifacts_cancellable(
        &self,
        request: ArtifactPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<ArtifactPage, EngineError> {
        validate_artifact_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Artifacts {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn usage_totals_cancellable(
        &self,
        request: UsageScopeRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<UsageTotalsReport, EngineError> {
        validate_usage_scope(&request)?;
        self.ensure_running()?;
        if cancellation.is_cancelled() {
            return Err(EngineError::QueryCancelled);
        }

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        self.enqueue(QueryCommand::UsageTotals {
            cancellation_epoch,
            cancellation,
            request,
            response: response_tx,
        })?;

        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    pub fn usage_activity(
        &self,
        request: UsageActivityRequest,
    ) -> Result<UsageActivityReport, EngineError> {
        self.usage_activity_cancellable(request, QueryCancellationToken::default())
    }

    pub fn usage_activity_cancellable(
        &self,
        request: UsageActivityRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<UsageActivityReport, EngineError> {
        validate_usage_activity(&request)?;
        self.ensure_running()?;
        if cancellation.is_cancelled() {
            return Err(EngineError::QueryCancelled);
        }

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        self.enqueue(QueryCommand::UsageActivity {
            cancellation_epoch,
            cancellation,
            request,
            response: response_tx,
        })?;

        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    pub fn runtime_usage_v2_cancellable(
        &self,
        request: RuntimeUsageV2PageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageV2Page, EngineError> {
        validate_runtime_usage_v2_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::RuntimeUsageV2 {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn runtime_usage_totals_cancellable(
        &self,
        request: RuntimeUsageTotalsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageTotalsReport, EngineError> {
        validate_runtime_usage_totals(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::RuntimeUsageTotals {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn runtime_usage_compatibility_cancellable(
        &self,
        request: RuntimeUsageCompatibilityRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageCompatibilityReport, EngineError> {
        validate_runtime_usage_compatibility(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::RuntimeUsageCompatibility {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub(crate) fn runtime_usage_query_selection_target_cancellable(
        &self,
        request: RuntimeUsageQuerySelectionTargetRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageQuerySelectionTarget, EngineError> {
        validate_runtime_usage_query_selection_target(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| {
                QueryCommand::RuntimeUsageQuerySelectionTarget {
                    cancellation_epoch,
                    cancellation,
                    request,
                    response,
                }
            },
        )
    }

    pub fn fact_family_coverage_cancellable(
        &self,
        request: FactFamilyCoveragePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<FactFamilyCoveragePage, EngineError> {
        validate_fact_family_coverage_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::FactFamilyCoverage {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub(crate) fn fact_family_replay_target_cancellable(
        &self,
        request: FactFamilyReplayTargetRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<FactFamilyReplayTarget, EngineError> {
        validate_fact_family_replay_target(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::FactFamilyReplayTarget {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn runtime_snapshot(
        &self,
        request: RuntimeSnapshotRequest,
    ) -> Result<RuntimeSnapshot, EngineError> {
        self.runtime_snapshot_cancellable(request, QueryCancellationToken::default())
    }

    pub fn run_state(&self, request: RunStateRequest) -> Result<RunStateLookup, EngineError> {
        self.run_state_cancellable(request, QueryCancellationToken::default())
    }

    pub fn run_state_cancellable(
        &self,
        request: RunStateRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RunStateLookup, EngineError> {
        validate_run_state_request(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::RunState {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn sources(&self, request: SourcePageRequest) -> Result<SourcePage, EngineError> {
        self.sources_cancellable(request, QueryCancellationToken::default())
    }

    pub fn sources_cancellable(
        &self,
        request: SourcePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<SourcePage, EngineError> {
        validate_source_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Sources {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn canonical_stats(&self) -> Result<CanonicalStats, EngineError> {
        self.canonical_stats_cancellable(QueryCancellationToken::default())
    }

    pub fn canonical_stats_cancellable(
        &self,
        cancellation: QueryCancellationToken,
    ) -> Result<CanonicalStats, EngineError> {
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::CanonicalStats {
                cancellation_epoch,
                cancellation,
                response,
            },
        )
    }

    pub fn runtime_snapshot_cancellable(
        &self,
        request: RuntimeSnapshotRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeSnapshot, EngineError> {
        validate_runtime_request(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::RuntimeSnapshot {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn teams(&self, request: TeamPageRequest) -> Result<TeamPage, EngineError> {
        self.teams_cancellable(request, QueryCancellationToken::default())
    }

    pub fn teams_cancellable(
        &self,
        request: TeamPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamPage, EngineError> {
        validate_team_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::Teams {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn team_details(&self, request: TeamDetailsRequest) -> Result<TeamDetails, EngineError> {
        self.team_details_cancellable(request, QueryCancellationToken::default())
    }

    pub fn team_details_cancellable(
        &self,
        request: TeamDetailsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamDetails, EngineError> {
        validate_team_details(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::TeamDetails {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn team_inboxes(
        &self,
        request: TeamInboxPageRequest,
    ) -> Result<TeamInboxPage, EngineError> {
        self.team_inboxes_cancellable(request, QueryCancellationToken::default())
    }

    pub fn team_inboxes_cancellable(
        &self,
        request: TeamInboxPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamInboxPage, EngineError> {
        validate_team_inbox_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::TeamInboxes {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub fn team_inbox_messages(
        &self,
        request: TeamInboxMessagePageRequest,
    ) -> Result<TeamInboxMessagePage, EngineError> {
        self.team_inbox_messages_cancellable(request, QueryCancellationToken::default())
    }

    pub fn team_inbox_messages_cancellable(
        &self,
        request: TeamInboxMessagePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamInboxMessagePage, EngineError> {
        validate_team_message_page(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::TeamInboxMessages {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    fn send_cancellable<T>(
        &self,
        cancellation: QueryCancellationToken,
        command: impl FnOnce(
            u64,
            QueryCancellationToken,
            Sender<Result<T, EngineError>>,
        ) -> QueryCommand,
    ) -> Result<T, EngineError> {
        self.ensure_running()?;
        if cancellation.is_cancelled() {
            return Err(EngineError::QueryCancelled);
        }
        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        self.enqueue(command(cancellation_epoch, cancellation, response_tx))?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    pub fn replay_changes(
        &self,
        request: ChangeReplayRequest,
    ) -> Result<ChangeReplay, EngineError> {
        self.replay_changes_cancellable(request, QueryCancellationToken::default())
    }

    pub fn replay_changes_cancellable(
        &self,
        request: ChangeReplayRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<ChangeReplay, EngineError> {
        validate_replay_request(&request)?;
        self.send_cancellable(
            cancellation,
            |cancellation_epoch, cancellation, response| QueryCommand::ReplayChanges {
                cancellation_epoch,
                cancellation,
                request,
                response,
            },
        )
    }

    pub(crate) fn catalog_projects(
        &self,
        request: CatalogProjectPageRequest,
    ) -> Result<CatalogProjectPage, EngineError> {
        self.send_cancellable(
            QueryCancellationToken::default(),
            |cancellation_epoch, cancellation, response| QueryCommand::CatalogProjects {
                cancellation_epoch,
                cancellation,
                request: Box::new(request),
                response,
            },
        )
    }

    pub(crate) fn catalog_sessions(
        &self,
        request: CatalogSessionPageRequest,
    ) -> Result<CatalogSessionPage, EngineError> {
        self.send_cancellable(
            QueryCancellationToken::default(),
            |cancellation_epoch, cancellation, response| QueryCommand::CatalogSessions {
                cancellation_epoch,
                cancellation,
                request: Box::new(request),
                response,
            },
        )
    }

    pub(crate) fn resolve_catalog_entity(
        &self,
        external_ref: String,
    ) -> Result<CatalogEntityResolution, EngineError> {
        self.send_cancellable(
            QueryCancellationToken::default(),
            |cancellation_epoch, cancellation, response| QueryCommand::CatalogResolve {
                cancellation_epoch,
                cancellation,
                external_ref,
                response,
            },
        )
    }

    pub(crate) fn readiness(&self) -> Result<Readiness, EngineError> {
        self.send_cancellable(
            QueryCancellationToken::default(),
            |cancellation_epoch, cancellation, response| QueryCommand::CatalogReadiness {
                cancellation_epoch,
                cancellation,
                response,
            },
        )
    }

    pub fn source_catalog(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
    ) -> Result<SourceCatalogSnapshot, EngineError> {
        if adapter_id.trim().is_empty() || stable_key.is_empty() {
            return Err(EngineError::InvalidQuery(
                "source catalog requires a non-empty adapter id and stable key".to_string(),
            ));
        }
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(EngineError::ShuttingDown);
        }

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        self.enqueue(QueryCommand::SourceCatalog {
            cancellation_epoch,
            adapter_id: adapter_id.to_string(),
            stable_key: stable_key.to_vec(),
            response: response_tx,
        })?;

        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    pub(crate) fn source_coverage_replay_baseline(
        &self,
        source_instance_id: u64,
        owner_id: &str,
        owner_scope_key: &[u8],
        family: &str,
        version: u32,
    ) -> Result<Option<SourceCoverageReplayBaseline>, EngineError> {
        if source_instance_id == 0
            || owner_id.trim().is_empty()
            || owner_scope_key.is_empty()
            || family.trim().is_empty()
            || version == 0
        {
            return Err(EngineError::InvalidQuery(
                "source coverage replay baseline requires a source instance, owner, scope, family, and positive version"
                    .to_string(),
            ));
        }
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(EngineError::ShuttingDown);
        }

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        // This is an engine-internal dependency of an already-admitted source
        // pass. Requiring a second permit could deadlock a capacity-one pool,
        // while placing permits on query workers could strand every worker
        // behind admitted source passes. The caller-held source permit remains
        // the enclosing resource authority for this bounded baseline read.
        self.enqueue_with_source_admission(
            false,
            QueryCommand::SourceCoverageReplayBaseline {
                cancellation_epoch,
                source_instance_id,
                owner_id: owner_id.to_string(),
                owner_scope_key: owner_scope_key.to_vec(),
                family: family.to_string(),
                version,
                response: response_tx,
            },
        )?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "query" })?
    }

    /// Cancel work that was submitted under the current epoch. New requests
    /// capture the incremented epoch and remain valid.
    pub fn cancel_pending(&self) -> u64 {
        self.control
            .cancellation_epoch
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }

    pub fn configured_workers(&self) -> usize {
        self.configured_workers
    }

    pub fn alive_workers(&self) -> usize {
        self.control.alive_workers.load(Ordering::Acquire)
    }

    pub fn in_flight(&self) -> usize {
        self.control.in_flight.load(Ordering::Acquire)
    }

    pub fn is_stopping(&self) -> bool {
        self.control.stopping.load(Ordering::Acquire)
    }

    pub fn performance_snapshot(&self) -> QueryPerformanceSnapshot {
        self.control.telemetry.snapshot(self.commands.len())
    }

    fn ensure_running(&self) -> Result<(), EngineError> {
        if self.control.stopping.load(Ordering::Acquire) {
            Err(EngineError::ShuttingDown)
        } else {
            Ok(())
        }
    }

    fn enqueue(&self, command: QueryCommand) -> Result<(), EngineError> {
        self.enqueue_with_source_admission(true, command)
    }

    fn enqueue_with_source_admission(
        &self,
        admit_source_pass: bool,
        command: QueryCommand,
    ) -> Result<(), EngineError> {
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(EngineError::ShuttingDown);
        }
        let queued_at = Instant::now();
        // Acquire before enqueueing so query workers never occupy every reader
        // thread while waiting behind source passes that need an internal read.
        let source_pass = admit_source_pass
            .then(|| {
                self.source_pass_pool
                    .as_ref()
                    .map(SharedSourcePassPool::blocking_acquire)
            })
            .flatten();
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(EngineError::ShuttingDown);
        }
        let queued = QueuedQuery::measured(command, queued_at, source_pass);
        let depth = self.commands.len().saturating_add(1);
        match self.commands.try_send(queued) {
            Ok(()) => {
                atomic_saturating_add(&self.control.telemetry.requests_enqueued, 1);
                atomic_max(
                    &self.control.telemetry.queue_high_watermark,
                    u64::try_from(depth).unwrap_or(u64::MAX),
                );
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                atomic_saturating_add(&self.control.telemetry.queue_rejections, 1);
                Err(EngineError::QueryQueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                Err(EngineError::WorkerUnavailable { worker: "query" })
            }
        }
    }

    #[cfg(test)]
    fn hold_worker(&self, entered: Sender<()>, release: Receiver<()>) {
        self.commands
            .send(QueuedQuery::control(QueryCommand::Hold {
                entered,
                release,
            }))
            .unwrap();
    }

    #[cfg(test)]
    fn hold_measured_worker(&self, entered: Sender<()>, release: Receiver<()>) {
        self.enqueue(QueryCommand::Hold { entered, release })
            .unwrap();
    }

    #[cfg(test)]
    fn hold_read_snapshot(&self, entered: Sender<Result<(), EngineError>>, release: Receiver<()>) {
        self.enqueue(QueryCommand::HoldReadSnapshot { entered, release })
            .unwrap();
    }

    #[cfg(test)]
    fn probe_write_rejected(&self) -> bool {
        let (tx, rx) = bounded(1);
        self.commands
            .send(QueuedQuery::control(QueryCommand::ProbeWrite(tx)))
            .unwrap();
        rx.recv().unwrap()
    }
}

pub struct QueryPool {
    client: QueryClient,
    joins: Vec<JoinHandle<()>>,
}

impl QueryPool {
    pub fn start(
        database_path: PathBuf,
        workers: usize,
        source_pass_pool: Option<SharedSourcePassPool>,
    ) -> Result<Self, EngineError> {
        let capacity = workers.saturating_mul(QUEUE_DEPTH_PER_WORKER).max(1);
        let (command_tx, command_rx) = bounded(capacity);
        let (ready_tx, ready_rx) = bounded(workers);
        let control = Arc::new(QueryControl::new(workers));
        let mut joins = Vec::with_capacity(workers);

        for worker_id in 0..workers {
            let thread_path = database_path.clone();
            let thread_commands = command_rx.clone();
            let thread_ready = ready_tx.clone();
            let thread_control = Arc::clone(&control);
            let join = thread::Builder::new()
                .name(format!("spaghetti-query-{worker_id}"))
                .spawn(move || {
                    query_thread(
                        worker_id,
                        thread_path,
                        thread_commands,
                        thread_ready,
                        thread_control,
                    )
                })
                .map_err(|error| EngineError::WorkerStart {
                    worker: "query",
                    detail: error.to_string(),
                })?;
            joins.push(join);
        }
        drop(ready_tx);

        let mut startup_error = None;
        for _ in 0..workers {
            match ready_rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    startup_error.get_or_insert(error);
                }
                Err(_) => {
                    startup_error.get_or_insert_with(|| EngineError::WorkerStart {
                        worker: "query",
                        detail: "query worker exited before reporting readiness".to_string(),
                    });
                }
            }
        }

        if let Some(error) = startup_error {
            control.stopping.store(true, Ordering::Release);
            for _ in 0..workers {
                let _ = command_tx.send(QueuedQuery::control(QueryCommand::Shutdown));
            }
            for join in joins {
                let _ = join.join();
            }
            return Err(error);
        }

        Ok(Self {
            client: QueryClient {
                commands: command_tx,
                control,
                configured_workers: workers,
                source_pass_pool,
            },
            joins,
        })
    }

    pub fn client(&self) -> QueryClient {
        self.client.clone()
    }

    pub fn shutdown(&mut self) -> Result<(), EngineError> {
        if self.joins.is_empty() {
            return Ok(());
        }

        self.client.control.stopping.store(true, Ordering::Release);
        self.client.cancel_pending();
        for _ in 0..self.joins.len() {
            let _ = self
                .client
                .commands
                .send(QueuedQuery::control(QueryCommand::Shutdown));
        }

        let mut panic_seen = false;
        for join in self.joins.drain(..) {
            panic_seen |= join.join().is_err();
        }
        if panic_seen {
            Err(EngineError::WorkerPanic { worker: "query" })
        } else {
            Ok(())
        }
    }
}

impl Drop for QueryPool {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn query_thread(
    worker_id: usize,
    database_path: PathBuf,
    commands: Receiver<QueuedQuery>,
    ready: Sender<Result<(), EngineError>>,
    control: Arc<QueryControl>,
) {
    let connection = match open_reader(&database_path) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    control.alive_workers.fetch_add(1, Ordering::AcqRel);
    if ready.send(Ok(())).is_err() {
        control.alive_workers.fetch_sub(1, Ordering::AcqRel);
        return;
    }
    // Do not keep the readiness channel alive while serving queries. If a
    // sibling panics before reporting startup, the opener must observe the
    // channel close instead of waiting forever for a missing message.
    drop(ready);

    while let Ok(queued) = commands.recv() {
        let QueuedQuery {
            command,
            queued_at,
            measured,
            _source_pass,
        } = queued;
        if matches!(command, QueryCommand::Shutdown) {
            break;
        }
        // The caller-acquired permit is deliberately retained for the whole
        // command even though the worker never waits for admission itself.
        let _measurement = measured
            .then(|| QueryMeasurementGuard::begin(&control.telemetry, worker_id, queued_at));
        match command {
            QueryCommand::Overview {
                cancellation_epoch,
                response,
            } => {
                if is_cancelled(&control, cancellation_epoch) {
                    let _ = response.send(Err(EngineError::QueryCancelled));
                    continue;
                }
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = read_overview(&connection).and_then(|overview| {
                    if is_cancelled(&control, cancellation_epoch) {
                        Err(EngineError::QueryCancelled)
                    } else {
                        Ok(overview)
                    }
                });
                let _ = response.send(result);
            }
            QueryCommand::HistoryProjects {
                cancellation_epoch,
                request,
                response,
            } => {
                if is_cancelled(&control, cancellation_epoch) {
                    let _ = response.send(Err(EngineError::QueryCancelled));
                    continue;
                }
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = super::history_query::read_history_projects(&connection, &request)
                    .and_then(|page| {
                        if is_cancelled(&control, cancellation_epoch) {
                            Err(EngineError::QueryCancelled)
                        } else {
                            Ok(page)
                        }
                    });
                let _ = response.send(result);
            }
            QueryCommand::HistorySessions {
                cancellation_epoch,
                request,
                response,
            } => {
                if is_cancelled(&control, cancellation_epoch) {
                    let _ = response.send(Err(EngineError::QueryCancelled));
                    continue;
                }
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = super::history_query::read_history_sessions(&connection, &request)
                    .and_then(|page| {
                        if is_cancelled(&control, cancellation_epoch) {
                            Err(EngineError::QueryCancelled)
                        } else {
                            Ok(page)
                        }
                    });
                let _ = response.send(result);
            }
            QueryCommand::SessionDetails {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_session_details(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Messages {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_message_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Search {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_search_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Timeline {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_timeline_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Delegations {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_delegation_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Workflows {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_workflow_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::WorkflowDetails {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_workflow_details(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::WorkflowMembers {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_workflow_member_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::MemoryDocuments {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_memory_document_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::TaskCollections {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_task_collection_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Tasks {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_task_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Plans {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_plan_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::ToolResults {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_tool_result_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Artifacts {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_artifact_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::UsageTotals {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_usage_totals(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::UsageActivity {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_usage_activity(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::RuntimeUsageV2 {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_runtime_usage_v2_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::RuntimeUsageTotals {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_runtime_usage_totals(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::RuntimeUsageCompatibility {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_runtime_usage_compatibility(&connection, &request),
                );
                if let Ok(report) = &result {
                    control.telemetry.runtime_usage_compatibility.record(report);
                }
                let _ = response.send(result);
            }
            QueryCommand::RuntimeUsageQuerySelectionTarget {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_runtime_usage_query_selection_target(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::FactFamilyCoverage {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_fact_family_coverage_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::FactFamilyReplayTarget {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_fact_family_replay_target(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::RuntimeSnapshot {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_runtime_snapshot(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::RunState {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_run_state(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::Sources {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_source_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::CanonicalStats {
                cancellation_epoch,
                cancellation,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_canonical_stats(&connection),
                );
                let _ = response.send(result);
            }
            QueryCommand::Teams {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_team_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::TeamDetails {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_team_details(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::TeamInboxes {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_team_inbox_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::TeamInboxMessages {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_team_message_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::ReplayChanges {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_change_replay(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::CatalogProjects {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_project_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::CatalogSessions {
                cancellation_epoch,
                cancellation,
                request,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_session_page(&connection, &request),
                );
                let _ = response.send(result);
            }
            QueryCommand::CatalogResolve {
                cancellation_epoch,
                cancellation,
                external_ref,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || resolve_catalog_entity(&connection, &external_ref),
                );
                let _ = response.send(result);
            }
            QueryCommand::CatalogReadiness {
                cancellation_epoch,
                cancellation,
                response,
            } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = run_cancellable_query(
                    &connection,
                    &control,
                    cancellation_epoch,
                    &cancellation,
                    || read_readiness(&connection),
                );
                let _ = response.send(result);
            }
            QueryCommand::SourceCatalog {
                cancellation_epoch,
                adapter_id,
                stable_key,
                response,
            } => {
                if is_cancelled(&control, cancellation_epoch) {
                    let _ = response.send(Err(EngineError::QueryCancelled));
                    continue;
                }
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = read_source_catalog(&connection, &adapter_id, &stable_key).and_then(
                    |catalog| {
                        if is_cancelled(&control, cancellation_epoch) {
                            Err(EngineError::QueryCancelled)
                        } else {
                            Ok(catalog)
                        }
                    },
                );
                let _ = response.send(result);
            }
            QueryCommand::SourceCoverageReplayBaseline {
                cancellation_epoch,
                source_instance_id,
                owner_id,
                owner_scope_key,
                family,
                version,
                response,
            } => {
                if is_cancelled(&control, cancellation_epoch) {
                    let _ = response.send(Err(EngineError::QueryCancelled));
                    continue;
                }
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = read_source_coverage_replay_baseline(
                    &connection,
                    source_instance_id,
                    &owner_id,
                    &owner_scope_key,
                    &family,
                    version,
                )
                .and_then(|baseline| {
                    if is_cancelled(&control, cancellation_epoch) {
                        Err(EngineError::QueryCancelled)
                    } else {
                        Ok(baseline)
                    }
                });
                let _ = response.send(result);
            }
            #[cfg(test)]
            QueryCommand::Hold { entered, release } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let _ = entered.send(());
                let _ = release.recv();
            }
            #[cfg(test)]
            QueryCommand::HoldReadSnapshot { entered, release } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let started = connection
                    .execute_batch("BEGIN DEFERRED")
                    .map_err(|error| query_sqlite_error("begin pinned reader test", error))
                    .and_then(|()| {
                        connection
                            .query_row("SELECT COUNT(*) FROM ingest_commits", [], |_| Ok(()))
                            .map_err(|error| {
                                query_sqlite_error("establish pinned reader snapshot", error)
                            })
                    });
                let ready = started.is_ok();
                let _ = entered.send(started);
                if ready {
                    let _ = release.recv();
                    let _ = connection.execute_batch("ROLLBACK");
                }
            }
            #[cfg(test)]
            QueryCommand::ProbeWrite(response) => {
                let rejected = connection
                    .execute_batch("CREATE TABLE rfc011_query_must_not_write(value INTEGER)")
                    .is_err();
                let _ = response.send(rejected);
            }
            QueryCommand::Shutdown => {
                unreachable!("shutdown is handled before the shared pass acquire")
            }
        }
    }

    control.alive_workers.fetch_sub(1, Ordering::AcqRel);
}

fn is_cancelled(control: &QueryControl, epoch: u64) -> bool {
    control.stopping.load(Ordering::Acquire)
        || control.cancellation_epoch.load(Ordering::Acquire) != epoch
}

fn run_cancellable_query<T>(
    connection: &Connection,
    control: &Arc<QueryControl>,
    cancellation_epoch: u64,
    cancellation: &QueryCancellationToken,
    query: impl FnOnce() -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    if is_cancelled(control, cancellation_epoch) || cancellation.is_cancelled() {
        return Err(EngineError::QueryCancelled);
    }
    let progress_control = Arc::clone(control);
    let progress_cancellation = cancellation.clone();
    connection.progress_handler(
        1_000,
        Some(move || {
            is_cancelled(&progress_control, cancellation_epoch)
                || progress_cancellation.is_cancelled()
        }),
    );
    let result = query();
    connection.progress_handler(0, None::<fn() -> bool>);
    if is_cancelled(control, cancellation_epoch) || cancellation.is_cancelled() {
        Err(EngineError::QueryCancelled)
    } else {
        result
    }
}

fn open_reader(database_path: &PathBuf) -> Result<Connection, EngineError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection =
        Connection::open_with_flags(database_path, flags).map_err(|error| EngineError::Sqlite {
            operation: "open read-only query connection",
            detail: error.to_string(),
        })?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| EngineError::Sqlite {
            operation: "configure query busy timeout",
            detail: error.to_string(),
        })?;
    connection
        .pragma_update(None, "query_only", "ON")
        .map_err(|error| EngineError::Sqlite {
            operation: "enable query_only",
            detail: error.to_string(),
        })?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| EngineError::Sqlite {
            operation: "enable query foreign keys",
            detail: error.to_string(),
        })?;
    connection
        .pragma_update(None, "cache_size", -32_000_i64)
        .map_err(|error| EngineError::Sqlite {
            operation: "configure query page cache",
            detail: error.to_string(),
        })?;
    connection
        .pragma_update(None, "mmap_size", 256_i64 * 1024 * 1024)
        .map_err(|error| EngineError::Sqlite {
            operation: "configure query mmap window",
            detail: error.to_string(),
        })?;
    // Catalog/history reads stay available while FTS bootstrap is incomplete.
    // Search remains complete-only in search_query::read_search_page.
    connection.set_prepared_statement_cache_capacity(128);
    Ok(connection)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum HistoryCursorKind {
    Projects,
    Sessions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct HistoryCursorPayload {
    pub(super) version: u32,
    pub(super) kind: HistoryCursorKind,
    pub(super) at_commit_seq: u64,
    pub(super) sort_time: String,
    pub(super) entity_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) project_id: Option<String>,
}

pub(super) struct HistoryProjectRow {
    pub(super) summary: HistoryProjectSummary,
    pub(super) project_key: Vec<u8>,
    pub(super) sort_time: String,
}

pub(super) struct HistorySessionRow {
    pub(super) summary: HistorySessionSummary,
    pub(super) session_key: Vec<u8>,
    pub(super) sort_time: String,
}

pub(super) fn validate_history_page_limit(limit: u32) -> Result<(), EngineError> {
    if !(1..=MAX_HISTORY_PAGE_LIMIT).contains(&limit) {
        return Err(EngineError::InvalidQuery(format!(
            "history page limit must be between 1 and {MAX_HISTORY_PAGE_LIMIT}, got {limit}"
        )));
    }
    Ok(())
}

pub(super) fn encode_history_cursor(cursor: &HistoryCursorPayload) -> Result<String, EngineError> {
    let json = serde_json::to_vec(cursor).map_err(|error| {
        EngineError::InvalidQuery(format!("could not encode history cursor: {error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

pub(super) fn decode_history_cursor(
    value: &str,
    expected_kind: HistoryCursorKind,
    expected_project_id: Option<&str>,
) -> Result<HistoryCursorPayload, EngineError> {
    if value.is_empty() || value.len() > MAX_HISTORY_TOKEN_BYTES {
        return Err(EngineError::InvalidQuery(
            "history cursor is empty or exceeds the supported bound".to_string(),
        ));
    }
    let json = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        EngineError::InvalidQuery("history cursor is not valid base64url".to_string())
    })?;
    let cursor: HistoryCursorPayload = serde_json::from_slice(&json).map_err(|_| {
        EngineError::InvalidQuery("history cursor payload is malformed".to_string())
    })?;
    if cursor.version != HISTORY_QUERY_CONTRACT_VERSION {
        return Err(EngineError::InvalidQuery(format!(
            "unsupported history cursor version {}",
            cursor.version
        )));
    }
    if cursor.kind != expected_kind || cursor.project_id.as_deref() != expected_project_id {
        return Err(EngineError::InvalidQuery(
            "history cursor does not belong to this query".to_string(),
        ));
    }
    if cursor.sort_time.len() > MAX_HISTORY_TOKEN_BYTES {
        return Err(EngineError::InvalidQuery(
            "history cursor order key exceeds the supported bound".to_string(),
        ));
    }
    let entity_key = URL_SAFE_NO_PAD.decode(&cursor.entity_key).map_err(|_| {
        EngineError::InvalidQuery("history cursor entity key is malformed".to_string())
    })?;
    if entity_key.is_empty() || entity_key.len() > MAX_HISTORY_TOKEN_BYTES {
        return Err(EngineError::InvalidQuery(
            "history cursor entity key is empty or exceeds the supported bound".to_string(),
        ));
    }
    Ok(cursor)
}

pub(super) fn cursor_entity_key(
    cursor: Option<&HistoryCursorPayload>,
) -> Result<Vec<u8>, EngineError> {
    cursor
        .map(|cursor| {
            URL_SAFE_NO_PAD.decode(&cursor.entity_key).map_err(|_| {
                EngineError::InvalidQuery("history cursor entity key is malformed".to_string())
            })
        })
        .transpose()
        .map(|key| key.unwrap_or_default())
}

pub(super) fn validate_history_cursor_watermark(
    cursor: Option<&HistoryCursorPayload>,
    current_watermark: u64,
) -> Result<(), EngineError> {
    if let Some(cursor) = cursor {
        if cursor.at_commit_seq != current_watermark {
            return Err(EngineError::InvalidQuery(format!(
                "history cursor expired at commit {}; current commit is {current_watermark}",
                cursor.at_commit_seq
            )));
        }
    }
    Ok(())
}

/// Decode the catalog reference a history row carries, if discovery has run.
fn read_overview(connection: &Connection) -> Result<QueryOverview, EngineError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin overview snapshot", error))?;
    let schema_version = schema::current_schema_version(&transaction)
        .map_err(|error| EngineError::Sqlite {
            operation: "read schema version",
            detail: error.to_string(),
        })?
        .unwrap_or(0);
    let query_only: i64 = transaction
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .map_err(|error| EngineError::Sqlite {
            operation: "verify query_only",
            detail: error.to_string(),
        })?;

    let commit_seq = read_commit_seq(&transaction)?;
    let change_log_oldest_cursor = read_oldest_change_cursor(&transaction, commit_seq)?;
    let (
        change_log_pruned_through_seq,
        change_log_retained_changes,
        change_log_retained_payload_bytes,
    ) = read_change_log_retention_state(&transaction)?;
    let overview = QueryOverview {
        schema_version,
        commit_seq,
        projects: count_table(&transaction, "projects")?,
        sessions: count_table(&transaction, "sessions")?,
        messages: count_table(&transaction, "messages")?,
        canonical_sessions: count_table(&transaction, "canonical_sessions")?,
        canonical_messages: count_table(&transaction, "canonical_messages")?,
        change_log_oldest_cursor,
        change_log_pruned_through_seq,
        change_log_retained_changes,
        change_log_retained_payload_bytes,
        query_only: query_only != 0,
        // The connection was opened with SQLITE_OPEN_READ_ONLY. The write
        // rejection test below verifies this invariant on the actual handle.
        read_only: true,
    };
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish overview snapshot", error))?;
    Ok(overview)
}

pub(super) fn read_source_catalog(
    connection: &Connection,
    adapter_id: &str,
    stable_key: &[u8],
) -> Result<SourceCatalogSnapshot, EngineError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin source catalog snapshot", error))?;
    let instance = transaction
        .query_row(
            r#"
            SELECT source_instance_id, adapter_contract_version
            FROM source_instances
            WHERE adapter_id = ?1 AND stable_key = ?2
            "#,
            rusqlite::params![adapter_id, stable_key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| query_sqlite_error("read source catalog instance", error))?;

    let Some((source_instance_id, adapter_contract_version)) = instance else {
        transaction
            .commit()
            .map_err(|error| query_sqlite_error("finish empty source catalog snapshot", error))?;
        return Ok(SourceCatalogSnapshot {
            source_instance_id: None,
            adapter_contract_version: None,
            streams: Vec::new(),
            objects: Vec::new(),
            projection_versions: Vec::new(),
        });
    };
    let source_instance_id = decode_nonnegative_u64(source_instance_id, "source instance id")?;
    let adapter_contract_version =
        decode_nonnegative_u32(adapter_contract_version, "adapter contract version")?;

    let streams = {
        let mut statement = transaction
            .prepare(
                r#"
                SELECT source_stream_id, stream_key, driver_kind, decoder_key,
                       stream_state, last_reconciled_at, last_commit_seq
                FROM source_streams
                WHERE source_instance_id = ?1
                ORDER BY stream_key
                "#,
            )
            .map_err(|error| query_sqlite_error("prepare source catalog streams", error))?;
        let rows = statement
            .query_map(
                [to_query_i64(source_instance_id, "source instance id")?],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .map_err(|error| query_sqlite_error("read source catalog streams", error))?;
        let mut streams = Vec::new();
        for row in rows {
            let (id, stream_key, driver_kind, decoder_key, stream_state, reconciled, commit) =
                row.map_err(|error| query_sqlite_error("decode source catalog stream", error))?;
            streams.push(SourceCatalogStream {
                source_stream_id: decode_nonnegative_u64(id, "source stream id")?,
                stream_key,
                driver_kind,
                decoder_key,
                stream_state,
                last_reconciled_at: reconciled,
                last_commit_seq: decode_optional_u64(commit, "source stream commit sequence")?,
            });
        }
        streams
    };

    let objects = {
        let mut statement = transaction
            .prepare(
                r#"
                SELECT so.source_stream_id, so.source_object_id, ss.stream_key,
                       so.object_key, so.display_path, so.native_identity,
                       so.generation, so.committed_cursor, so.observed_revision,
                       so.adapter_object_context, so.driver_checkpoint,
                       so.driver_checkpoint_version, so.decoder_state,
                       so.decoder_state_version, so.retry_state,
                       so.size_bytes, so.mtime_ns,
                       so.decoder_contract_version, so.last_commit_seq, so.state
                FROM source_objects so
                JOIN source_streams ss ON ss.source_stream_id = so.source_stream_id
                WHERE ss.source_instance_id = ?1
                ORDER BY ss.stream_key, so.object_key
                "#,
            )
            .map_err(|error| query_sqlite_error("prepare source catalog objects", error))?;
        let mut rows = statement
            .query([to_query_i64(source_instance_id, "source instance id")?])
            .map_err(|error| query_sqlite_error("read source catalog objects", error))?;
        let mut objects = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| query_sqlite_error("advance source catalog objects", error))?
        {
            let driver_checkpoint = row
                .get::<_, Option<Vec<u8>>>(10)
                .map_err(|error| query_sqlite_error("decode driver checkpoint", error))?;
            let driver_checkpoint_version = decode_optional_u32(
                row.get::<_, Option<i64>>(11).map_err(|error| {
                    query_sqlite_error("decode driver checkpoint version", error)
                })?,
                "driver checkpoint version",
            )?;
            if driver_checkpoint.is_some() != driver_checkpoint_version.is_some() {
                return Err(EngineError::Sqlite {
                    operation: "validate source catalog driver checkpoint",
                    detail: "driver checkpoint and version presence disagree".to_string(),
                });
            }
            objects.push(SourceCatalogObject {
                source_stream_id: decode_nonnegative_u64(
                    row.get(0)
                        .map_err(|error| query_sqlite_error("decode source stream id", error))?,
                    "source stream id",
                )?,
                source_object_id: decode_nonnegative_u64(
                    row.get(1)
                        .map_err(|error| query_sqlite_error("decode source object id", error))?,
                    "source object id",
                )?,
                stream_key: row
                    .get(2)
                    .map_err(|error| query_sqlite_error("decode source stream key", error))?,
                object_key: row
                    .get(3)
                    .map_err(|error| query_sqlite_error("decode source object key", error))?,
                display_path: row
                    .get(4)
                    .map_err(|error| query_sqlite_error("decode source display path", error))?,
                native_identity: row
                    .get(5)
                    .map_err(|error| query_sqlite_error("decode native identity", error))?,
                generation: decode_nonnegative_u64(
                    row.get(6)
                        .map_err(|error| query_sqlite_error("decode source generation", error))?,
                    "source generation",
                )?,
                committed_cursor: row
                    .get(7)
                    .map_err(|error| query_sqlite_error("decode committed cursor", error))?,
                observed_revision: row
                    .get(8)
                    .map_err(|error| query_sqlite_error("decode observed revision", error))?,
                adapter_object_context: row
                    .get(9)
                    .map_err(|error| query_sqlite_error("decode adapter object context", error))?,
                driver_checkpoint,
                driver_checkpoint_version,
                decoder_state: row
                    .get(12)
                    .map_err(|error| query_sqlite_error("decode adapter state", error))?,
                decoder_state_version: decode_optional_u32(
                    row.get(13).map_err(|error| {
                        query_sqlite_error("decode adapter state version", error)
                    })?,
                    "adapter state version",
                )?,
                retry_state: row
                    .get(14)
                    .map_err(|error| query_sqlite_error("decode source retry state", error))?,
                size_bytes: decode_optional_u64(
                    row.get(15)
                        .map_err(|error| query_sqlite_error("decode source size", error))?,
                    "source size",
                )?,
                mtime_ns: row
                    .get(16)
                    .map_err(|error| query_sqlite_error("decode source mtime", error))?,
                decoder_contract_version: decode_nonnegative_u32(
                    row.get(17).map_err(|error| {
                        query_sqlite_error("decode decoder contract version", error)
                    })?,
                    "decoder contract version",
                )?,
                last_commit_seq: decode_optional_u64(
                    row.get(18)
                        .map_err(|error| query_sqlite_error("decode source commit", error))?,
                    "source commit sequence",
                )?,
                state: row
                    .get(19)
                    .map_err(|error| query_sqlite_error("decode source state", error))?,
            });
        }
        objects
    };

    let projection_versions = {
        let mut statement = transaction
            .prepare(
                r#"
                SELECT projection_id, desired_version, completed_version,
                       readiness, last_commit_seq, updated_at, detail
                FROM projection_versions
                WHERE scope_key = ?1
                ORDER BY projection_id
                "#,
            )
            .map_err(|error| query_sqlite_error("prepare source projection versions", error))?;
        let rows = statement
            .query_map([stable_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|error| query_sqlite_error("read source projection versions", error))?;
        let mut versions = Vec::new();
        for row in rows {
            let (projection_id, desired, completed, readiness, commit, updated_at, detail) =
                row.map_err(|error| query_sqlite_error("decode source projection version", error))?;
            versions.push(SourceCatalogProjectionVersion {
                projection_id,
                desired_version: decode_nonnegative_u32(desired, "projection desired version")?,
                completed_version: completed
                    .map(|value| decode_nonnegative_u32(value, "projection completed version"))
                    .transpose()?,
                readiness,
                last_commit_seq: decode_optional_u64(commit, "projection commit sequence")?,
                updated_at,
                detail,
            });
        }
        versions
    };

    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish source catalog snapshot", error))?;
    Ok(SourceCatalogSnapshot {
        source_instance_id: Some(source_instance_id),
        adapter_contract_version: Some(adapter_contract_version),
        streams,
        objects,
        projection_versions,
    })
}

fn read_source_coverage_replay_baseline(
    connection: &Connection,
    source_instance_id: u64,
    owner_id: &str,
    owner_scope_key: &[u8],
    family: &str,
    version: u32,
) -> Result<Option<SourceCoverageReplayBaseline>, EngineError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin source coverage replay baseline", error))?;
    let set = transaction
        .query_row(
            r#"
            SELECT coverage_set_id, adapter_id, canonical_source_instance_key,
                   completeness, content_digest, last_commit_seq
            FROM source_coverage_sets
            WHERE source_instance_id = ?1
              AND owner_id = ?2
              AND owner_scope_key = ?3
              AND domain_kind = 'fact_family'
              AND domain_name = ?4
              AND domain_version = ?5
              AND root_entity_key = X''
            "#,
            rusqlite::params![
                to_query_i64(source_instance_id, "source coverage instance")?,
                owner_id,
                owner_scope_key,
                family,
                i64::from(version),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| query_sqlite_error("read source coverage replay set", error))?;
    let Some((
        coverage_set_id,
        adapter_id,
        canonical_source_instance_key,
        completeness,
        content_digest,
        last_commit_seq,
    )) = set
    else {
        transaction.commit().map_err(|error| {
            query_sqlite_error("finish empty source coverage replay baseline", error)
        })?;
        return Ok(None);
    };
    if canonical_source_instance_key.len() != 32 || content_digest.len() != 32 {
        return Err(EngineError::Sqlite {
            operation: "validate source coverage replay baseline",
            detail: "coverage baseline contains an invalid common identity".to_string(),
        });
    }

    let mut statement = transaction
        .prepare(
            r#"
            SELECT stream_key, object_key, generation, 0 AS absent
            FROM source_coverage_points
            WHERE coverage_set_id = ?1
            UNION ALL
            SELECT stream_key, object_key, generation, 1 AS absent
            FROM source_coverage_absences
            WHERE coverage_set_id = ?1
            ORDER BY stream_key, object_key, generation, absent
            "#,
        )
        .map_err(|error| query_sqlite_error("prepare source coverage replay members", error))?;
    let rows = statement
        .query_map([coverage_set_id], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| query_sqlite_error("read source coverage replay members", error))?;
    let mut members = Vec::new();
    for row in rows {
        let (stream_key, object_key, generation, absent) =
            row.map_err(|error| query_sqlite_error("decode source coverage replay member", error))?;
        if stream_key.len() != 32 || object_key.len() != 32 || !matches!(absent, 0 | 1) {
            return Err(EngineError::Sqlite {
                operation: "validate source coverage replay member",
                detail: "coverage member has an invalid key or absence marker".to_string(),
            });
        }
        members.push(SourceCoverageReplayMember {
            stream_key,
            object_key,
            generation: decode_nonnegative_u64(generation, "coverage replay generation")?,
            absent: absent == 1,
        });
    }
    if members.windows(2).any(|pair| {
        pair[0].stream_key == pair[1].stream_key && pair[0].object_key == pair[1].object_key
    }) {
        return Err(EngineError::Sqlite {
            operation: "validate source coverage replay baseline",
            detail: "coverage baseline contains duplicate stream/object membership".to_string(),
        });
    }
    drop(statement);
    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish source coverage replay baseline", error))?;
    Ok(Some(SourceCoverageReplayBaseline {
        source_instance_id,
        adapter_id,
        canonical_source_instance_key,
        completeness,
        content_digest,
        last_commit_seq: decode_nonnegative_u64(
            last_commit_seq,
            "source coverage replay commit sequence",
        )?,
        members,
    }))
}

fn count_table(connection: &Connection, table: &'static str) -> Result<u32, EngineError> {
    let sql = match table {
        "projects" => "SELECT COUNT(*) FROM projects",
        "sessions" => "SELECT COUNT(*) FROM sessions",
        "messages" => "SELECT COUNT(*) FROM messages",
        "canonical_sessions" => "SELECT COUNT(*) FROM canonical_sessions",
        "canonical_messages" => "SELECT COUNT(*) FROM canonical_messages",
        _ => unreachable!("count_table only accepts fixed schema names"),
    };
    let count: i64 = connection
        .query_row(sql, [], |row| row.get(0))
        .map_err(|error| EngineError::Sqlite {
            operation: "count query rows",
            detail: error.to_string(),
        })?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

fn read_commit_seq(connection: &Connection) -> Result<u64, EngineError> {
    let table_exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'ingest_commits'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| EngineError::Sqlite {
            operation: "discover commit watermark",
            detail: error.to_string(),
        })?;
    if table_exists == 0 {
        return Ok(0);
    }

    read_committed_watermark(connection)
}

const MAX_REPLAY_CHANGES: u32 = 1_000;
const MAX_REPLAY_TOPICS: usize = 64;

fn validate_replay_request(request: &ChangeReplayRequest) -> Result<(), EngineError> {
    if !(1..=MAX_REPLAY_CHANGES).contains(&request.limit) {
        return Err(EngineError::InvalidQuery(format!(
            "change replay limit must be between 1 and {MAX_REPLAY_CHANGES}, got {}",
            request.limit
        )));
    }
    if request.topics.iter().any(|topic| topic.trim().is_empty()) {
        return Err(EngineError::InvalidQuery(
            "change replay topics must not contain empty values".to_string(),
        ));
    }
    if request.topics.len() > MAX_REPLAY_TOPICS {
        return Err(EngineError::InvalidQuery(format!(
            "change replay accepts at most {MAX_REPLAY_TOPICS} topics, got {}",
            request.topics.len()
        )));
    }
    Ok(())
}

fn read_change_replay(
    connection: &Connection,
    request: &ChangeReplayRequest,
) -> Result<ChangeReplay, EngineError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| query_sqlite_error("begin change replay snapshot", error))?;
    let watermark = read_committed_watermark(&transaction)?;
    let oldest_available = read_oldest_change_cursor(&transaction, watermark)?;
    let (pruned_through_commit_seq, _, _) = read_change_log_retention_state(&transaction)?;
    if request
        .after
        .is_some_and(|cursor| cursor.commit_seq <= pruned_through_commit_seq)
        && pruned_through_commit_seq > 0
    {
        return Err(EngineError::ResetRequired {
            current_commit_seq: watermark,
            oldest_commit_seq: oldest_available.map(|cursor| cursor.commit_seq),
            oldest_ordinal: oldest_available.map(|cursor| cursor.ordinal),
        });
    }

    let after = request.after.unwrap_or(ChangeCursor {
        commit_seq: 0,
        ordinal: 0,
    });
    let mut arguments = vec![
        Value::Integer(to_query_i64(after.commit_seq, "change cursor sequence")?),
        Value::Integer(i64::from(after.ordinal)),
        Value::Integer(to_query_i64(watermark, "commit watermark")?),
    ];
    let mut sql = String::from(
        r#"
        SELECT commit_seq, ordinal, topic, schema_version, entity_key, operation, payload
        FROM change_log
        WHERE (commit_seq > ?1 OR (commit_seq = ?1 AND ordinal > ?2))
          AND commit_seq <= ?3
        "#,
    );
    if !request.topics.is_empty() {
        sql.push_str(" AND topic IN (");
        for (index, topic) in request.topics.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            let parameter = arguments.len() + 1;
            sql.push('?');
            sql.push_str(&parameter.to_string());
            arguments.push(Value::Text(topic.clone()));
        }
        sql.push(')');
    }
    let limit_parameter = arguments.len() + 1;
    sql.push_str(" ORDER BY commit_seq, ordinal LIMIT ?");
    sql.push_str(&limit_parameter.to_string());
    arguments.push(Value::Integer(i64::from(request.limit) + 1));

    let (changes, payload_bytes, has_more) = {
        let mut statement = transaction
            .prepare(&sql)
            .map_err(|error| query_sqlite_error("prepare change replay", error))?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(arguments.iter()))
            .map_err(|error| query_sqlite_error("execute change replay", error))?;
        let mut changes = Vec::new();
        let mut payload_bytes = 0_u64;
        let mut has_more = false;
        while let Some(row) = rows
            .next()
            .map_err(|error| query_sqlite_error("read durable change", error))?
        {
            if changes.len() >= request.limit as usize {
                has_more = true;
                break;
            }
            let commit_seq: i64 = row
                .get(0)
                .map_err(|error| query_sqlite_error("decode change sequence", error))?;
            let ordinal: i64 = row
                .get(1)
                .map_err(|error| query_sqlite_error("decode change ordinal", error))?;
            let schema_version: i64 = row
                .get(3)
                .map_err(|error| query_sqlite_error("decode change schema version", error))?;
            let change = DurableChange {
                cursor: decode_cursor(commit_seq, ordinal)?,
                topic: row
                    .get(2)
                    .map_err(|error| query_sqlite_error("decode change topic", error))?,
                schema_version: u32::try_from(schema_version).map_err(|_| EngineError::Sqlite {
                    operation: "decode change schema version",
                    detail: format!("schema version was outside u32: {schema_version}"),
                })?,
                entity_key: row
                    .get(4)
                    .map_err(|error| query_sqlite_error("decode change entity key", error))?,
                operation: row
                    .get(5)
                    .map_err(|error| query_sqlite_error("decode change operation", error))?,
                payload: row
                    .get(6)
                    .map_err(|error| query_sqlite_error("decode change payload", error))?,
            };
            let change_bytes = change_payload_bytes(&change)?;
            let next_bytes =
                payload_bytes
                    .checked_add(change_bytes)
                    .ok_or_else(|| EngineError::Sqlite {
                        operation: "bound change replay payload",
                        detail: "change replay payload byte total overflowed u64".to_string(),
                    })?;
            if next_bytes > MAX_CHANGE_REPLAY_PAYLOAD_BYTES {
                if changes.is_empty() {
                    return Err(EngineError::Sqlite {
                        operation: "bound change replay payload",
                        detail: format!(
                            "one durable change requires {change_bytes} payload bytes; maximum is {MAX_CHANGE_REPLAY_PAYLOAD_BYTES}"
                        ),
                    });
                }
                has_more = true;
                break;
            }
            payload_bytes = next_bytes;
            changes.push(change);
        }
        (changes, payload_bytes, has_more)
    };

    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish change replay snapshot", error))?;

    let next_cursor = changes.last().map(|change| change.cursor);
    Ok(ChangeReplay {
        contract_version: CHANGE_REPLAY_CONTRACT_VERSION,
        at_commit_seq: watermark,
        oldest_available,
        changes,
        next_cursor,
        has_more,
        payload_bytes,
        payload_byte_limit: MAX_CHANGE_REPLAY_PAYLOAD_BYTES,
    })
}

fn read_oldest_change_cursor(
    connection: &Connection,
    watermark: u64,
) -> Result<Option<ChangeCursor>, EngineError> {
    connection
        .query_row(
            r#"
            SELECT commit_seq, ordinal
            FROM change_log
            WHERE commit_seq <= ?1
            ORDER BY commit_seq, ordinal
            LIMIT 1
            "#,
            [to_query_i64(watermark, "commit watermark")?],
            |row| {
                let commit_seq: i64 = row.get(0)?;
                let ordinal: i64 = row.get(1)?;
                Ok((commit_seq, ordinal))
            },
        )
        .optional()
        .map_err(|error| query_sqlite_error("read oldest durable change", error))?
        .map(|(commit_seq, ordinal)| decode_cursor(commit_seq, ordinal))
        .transpose()
}

fn read_change_log_retention_state(
    connection: &Connection,
) -> Result<(u64, u64, u64), EngineError> {
    let (floor, retained_changes, retained_payload_bytes): (i64, i64, i64) = connection
        .query_row(
            r#"
            SELECT pruned_through_commit_seq, retained_change_count,
                   retained_payload_bytes
            FROM change_log_retention_state WHERE singleton = 1
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| query_sqlite_error("read change-log retention metrics", error))?;
    Ok((
        decode_nonnegative_i64(floor, "change-log pruned floor")?,
        decode_nonnegative_i64(retained_changes, "retained change count")?,
        decode_nonnegative_i64(retained_payload_bytes, "retained change payload bytes")?,
    ))
}

fn decode_nonnegative_i64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode non-negative query metric",
        detail: format!("{field} was negative: {value}"),
    })
}

fn change_payload_bytes(change: &DurableChange) -> Result<u64, EngineError> {
    [
        change.topic.len(),
        change.entity_key.len(),
        change.operation.len(),
        change.payload.len(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, bytes| {
        let bytes = u64::try_from(bytes).map_err(|_| EngineError::Sqlite {
            operation: "bound change replay payload",
            detail: "change replay field length exceeded u64".to_string(),
        })?;
        total.checked_add(bytes).ok_or_else(|| EngineError::Sqlite {
            operation: "bound change replay payload",
            detail: "change replay row payload byte total overflowed u64".to_string(),
        })
    })
}

pub(super) fn read_committed_watermark(connection: &Connection) -> Result<u64, EngineError> {
    let value: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(commit_seq), 0) FROM ingest_commits WHERE committed_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|error| query_sqlite_error("read committed snapshot watermark", error))?;
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode committed snapshot watermark",
        detail: format!("commit watermark was negative: {value}"),
    })
}

fn decode_cursor(commit_seq: i64, ordinal: i64) -> Result<ChangeCursor, EngineError> {
    Ok(ChangeCursor {
        commit_seq: u64::try_from(commit_seq).map_err(|_| EngineError::Sqlite {
            operation: "decode durable change cursor",
            detail: format!("commit sequence was negative: {commit_seq}"),
        })?,
        ordinal: u32::try_from(ordinal).map_err(|_| EngineError::Sqlite {
            operation: "decode durable change cursor",
            detail: format!("change ordinal was outside u32: {ordinal}"),
        })?,
    })
}

pub(super) fn decode_nonnegative_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode source catalog integer",
        detail: format!("{field} was negative: {value}"),
    })
}

fn decode_nonnegative_u32(value: i64, field: &'static str) -> Result<u32, EngineError> {
    u32::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode source catalog integer",
        detail: format!("{field} was outside u32: {value}"),
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
        .map(|value| decode_nonnegative_u32(value, field))
        .transpose()
}

fn to_query_i64(value: u64, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidQuery(format!("{field} exceeds SQLite integer range")))
}

pub(super) fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::commit::{
        ChangeEntry, ChangeLogRetentionPolicy, ExpectedSourceCursor, ObservationCommit,
        SourceInstanceSpec, SourceObjectUpdate, SourceStreamSpec,
    };
    use crate::engine::writer::{WriterClient, WriterRuntime};
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn commit_request() -> ObservationCommit {
        ObservationCommit {
            source: SourceInstanceSpec {
                adapter_id: "query-fixture".to_string(),
                stable_key: b"root".to_vec(),
                display_name: "Query fixture".to_string(),
                adapter_version: "1.0.0".to_string(),
                adapter_contract_version: 1,
                source_schema_versions: Vec::new(),
                capabilities: Vec::new(),
                discovered_at: 10,
                last_seen_at: 10,
            },
            stream: SourceStreamSpec {
                stream_key: "history".to_string(),
                driver_kind: "append_file".to_string(),
                decoder_key: "fixture".to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: None,
                consistency: crate::adapter::ConsistencyPolicy::IncrementalCursor,
                retention: crate::adapter::RawRetentionPolicy::Full,
            },
            object: SourceObjectUpdate {
                object_key: b"object".to_vec(),
                expected: ExpectedSourceCursor::Absent,
                display_path: None,
                native_identity: None,
                generation: 1,
                committed_cursor: b"cursor".to_vec(),
                observed_revision: None,
                adapter_object_context: None,
                driver_checkpoint: None,
                driver_checkpoint_version: None,
                decoder_state: None,
                decoder_state_version: None,
                retry_state: None,
                size_bytes: None,
                mtime_ns: None,
                decoder_contract_version: 1,
                state: "active".to_string(),
            },
            reason: "test".to_string(),
            started_at: 10,
            committed_at: 11,
            fact_count: 1,
            projection_versions: Vec::new(),
            record_errors: Vec::new(),
            changes: vec![
                ChangeEntry {
                    topic: "history.session.changed".to_string(),
                    schema_version: 1,
                    entity_key: b"session".to_vec(),
                    operation: "upsert".to_string(),
                    payload: b"history".to_vec(),
                },
                ChangeEntry {
                    topic: "runtime.session.changed".to_string(),
                    schema_version: 1,
                    entity_key: b"session".to_vec(),
                    operation: "upsert".to_string(),
                    payload: b"runtime".to_vec(),
                },
            ],
        }
    }

    fn commit_revisions(client: &WriterClient, count: u8) {
        let mut previous_cursor: Option<Vec<u8>> = None;
        for sequence in 1..=count {
            let mut request = commit_request();
            request.object.expected = previous_cursor
                .as_ref()
                .map(|cursor| ExpectedSourceCursor::At {
                    generation: 1,
                    committed_cursor: cursor.clone(),
                })
                .unwrap_or(ExpectedSourceCursor::Absent);
            let cursor = vec![sequence];
            request.object.committed_cursor = cursor.clone();
            request.started_at = i64::from(sequence) * 10;
            request.committed_at = request.started_at + 1;
            request.changes[0].payload = vec![sequence];
            request.changes[1].payload = vec![sequence];
            let receipt = client.commit_observation(request).unwrap();
            assert_eq!(receipt.commit_seq, u64::from(sequence));
            previous_cursor = Some(cursor);
        }
    }

    #[test]
    fn overview_is_typed_read_only_and_does_not_change_database_content() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("query.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database.clone(), 2, None).unwrap();
        let client = pool.client();

        let probe = Connection::open(&database).unwrap();
        let before: i64 = probe
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();
        let overview = client.overview().unwrap();
        let page = HistoryProjectPageRequest {
            cursor: None,
            limit: DEFAULT_HISTORY_PAGE_LIMIT,
        };
        let projects = client.history_projects(page).unwrap();
        let missing_project_id = encode_entity_id(PROJECT_ID_PREFIX, b"missing-project");
        let page = HistorySessionPageRequest {
            project_id: missing_project_id.clone(),
            cursor: None,
            limit: DEFAULT_HISTORY_PAGE_LIMIT,
        };
        let sessions = client.history_sessions(page).unwrap();
        let missing_session_id = encode_entity_id(SESSION_ID_PREFIX, b"missing-session");
        let session_details = client
            .session_details(SessionDetailsRequest {
                session_id: missing_session_id.clone(),
            })
            .unwrap();
        let messages = client
            .messages(MessagePageRequest {
                project_id: missing_project_id.clone(),
                session_id: missing_session_id.clone(),
                cursor: None,
                limit: DEFAULT_HISTORY_PAGE_LIMIT,
            })
            .unwrap_err();
        let search = client
            .search_cancellable(
                SearchPageRequest {
                    text: "missing phrase".to_string(),
                    project_id: None,
                    session_id: None,
                    adapter_ids: Vec::new(),
                    roles: Vec::new(),
                    native_kinds: Vec::new(),
                    branch_kind: None,
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        let sources = client
            .sources(SourcePageRequest {
                cursor: None,
                limit: DEFAULT_HISTORY_PAGE_LIMIT,
            })
            .unwrap();
        let canonical_stats = client.canonical_stats().unwrap();
        let run_state = client
            .run_state(RunStateRequest {
                run_id: encode_entity_id(
                    super::super::query_identity::RUN_ID_PREFIX,
                    b"missing-run",
                ),
            })
            .unwrap();
        let usage = client
            .usage_totals(UsageScopeRequest {
                project_id: missing_project_id.clone(),
                session_id: None,
            })
            .unwrap();
        let usage_activity = client
            .usage_activity(UsageActivityRequest {
                project_id: missing_project_id.clone(),
                session_id: None,
                from: "2026-08-12".to_string(),
                to: "2026-08-12".to_string(),
            })
            .unwrap();
        let runtime = client
            .runtime_snapshot(RuntimeSnapshotRequest {
                project_id: None,
                session_id: None,
                cursor: None,
                limit: DEFAULT_HISTORY_PAGE_LIMIT,
            })
            .unwrap();
        let teams = client
            .teams(TeamPageRequest {
                cursor: None,
                limit: DEFAULT_HISTORY_PAGE_LIMIT,
            })
            .unwrap();
        let memory_documents = client
            .memory_documents_cancellable(
                MemoryDocumentPageRequest {
                    project_id: missing_project_id.clone(),
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        let task_collections = client
            .task_collections_cancellable(
                TaskCollectionPageRequest {
                    session_id: None,
                    run_id: None,
                    team_id: None,
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        let tasks = client
            .tasks_cancellable(
                TaskPageRequest {
                    collection_id: encode_entity_id(
                        super::super::query_identity::TASK_COLLECTION_ID_PREFIX,
                        b"missing-collection",
                    ),
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        let plans = client
            .plans_cancellable(
                PlanPageRequest {
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        let tool_results = client
            .tool_results_cancellable(
                ToolResultPageRequest {
                    project_id: missing_project_id,
                    session_id: missing_session_id.clone(),
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                QueryCancellationToken::default(),
            )
            .unwrap_err();
        let artifacts = client
            .artifacts_cancellable(
                ArtifactPageRequest {
                    session_id: missing_session_id,
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        let after: i64 = probe
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();

        assert_eq!(overview.schema_version, schema::SCHEMA_VERSION);
        assert_eq!(overview.projects, 0);
        assert_eq!(overview.sessions, 0);
        assert_eq!(overview.messages, 0);
        assert_eq!(overview.canonical_sessions, 0);
        assert_eq!(overview.canonical_messages, 0);
        assert_eq!(projects.at_commit_seq, overview.commit_seq);
        assert!(projects.items.is_empty());
        assert_eq!(sessions.at_commit_seq, overview.commit_seq);
        assert!(sessions.items.is_empty());
        assert_eq!(session_details.at_commit_seq, overview.commit_seq);
        assert!(session_details.session.is_none());
        assert!(matches!(messages, EngineError::InvalidQuery(_)));
        assert_eq!(search.at_commit_seq, overview.commit_seq);
        assert_eq!(search.total, 0);
        assert!(search.items.is_empty());
        assert_eq!(sources.at_commit_seq, overview.commit_seq);
        assert!(sources.items.is_empty());
        assert_eq!(canonical_stats.at_commit_seq, overview.commit_seq);
        assert_eq!(canonical_stats.source_instances, 0);
        assert_eq!(run_state.at_commit_seq, overview.commit_seq);
        assert!(run_state.run.is_none());
        assert_eq!(usage.at_commit_seq, overview.commit_seq);
        assert_eq!(usage.aggregate.quality, "unavailable");
        assert_eq!(usage.aggregate.contribution_count, 0);
        assert!(usage.coverage.is_empty());
        assert_eq!(usage_activity.at_commit_seq, overview.commit_seq);
        assert!(usage_activity.days.is_empty());
        assert_eq!(usage_activity.aggregate.contribution_count, 0);
        assert_eq!(usage_activity.untimed.aggregate.contribution_count, 0);
        assert_eq!(runtime.at_commit_seq, overview.commit_seq);
        assert!(runtime.entries.is_empty());
        assert_eq!(teams.at_commit_seq, overview.commit_seq);
        assert!(teams.items.is_empty());
        assert_eq!(memory_documents.at_commit_seq, overview.commit_seq);
        assert!(memory_documents.items.is_empty());
        assert_eq!(task_collections.at_commit_seq, overview.commit_seq);
        assert!(task_collections.items.is_empty());
        assert_eq!(tasks.at_commit_seq, overview.commit_seq);
        assert!(tasks.items.is_empty());
        assert_eq!(plans.at_commit_seq, overview.commit_seq);
        assert!(plans.items.is_empty());
        assert!(matches!(tool_results, EngineError::InvalidQuery(_)));
        assert_eq!(artifacts.at_commit_seq, overview.commit_seq);
        assert!(artifacts.items.is_empty());
        assert!(overview.query_only && overview.read_only);
        assert_eq!(before, after, "a query must not advance database content");
        let performance = client.performance_snapshot();
        assert!(performance.requests_enqueued > 0);
        assert!(performance.requests_completed > 0);
        assert_eq!(performance.queue_rejections, 0);
        assert_eq!(
            performance
                .timings
                .iter()
                .find(|timing| timing.name == "execution")
                .unwrap()
                .latency
                .samples,
            performance.requests_completed,
        );
        assert!(client.probe_write_rejected());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn cancellation_epoch_rejects_queued_work_but_accepts_new_work() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();

        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        client.hold_worker(entered_tx, release_rx);
        entered_rx.recv().unwrap();

        let queued_client = client.clone();
        let queued = thread::spawn(move || {
            let page = HistoryProjectPageRequest {
                cursor: None,
                limit: DEFAULT_HISTORY_PAGE_LIMIT,
            };
            queued_client.history_projects(page)
        });
        while client.commands.is_empty() {
            thread::yield_now();
        }
        client.cancel_pending();
        release_tx.send(()).unwrap();

        assert!(matches!(
            queued.join().unwrap(),
            Err(EngineError::QueryCancelled)
        ));
        assert!(
            client.overview().is_ok(),
            "new epoch should accept new work"
        );

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn performance_snapshot_reports_active_reader_age() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("active-reader.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();

        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        client.hold_measured_worker(entered_tx, release_rx);
        entered_rx.recv().unwrap();
        thread::sleep(Duration::from_millis(2));

        let active = client.performance_snapshot();
        assert_eq!(active.requests_enqueued, 1);
        assert_eq!(active.requests_completed, 0);
        assert!(active.oldest_active_ns >= 1_000_000);

        release_tx.send(()).unwrap();
        client.overview().unwrap();
        let released = client.performance_snapshot();
        assert_eq!(released.requests_enqueued, 2);
        assert_eq!(released.requests_completed, 2);
        assert_eq!(released.oldest_active_ns, 0);

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn checkpoint_reports_pinned_reader_and_reclaims_wal_after_release() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("pinned-reader.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let writer_client = writer.client();
        let mut pool = QueryPool::start(database.clone(), 1, None).unwrap();
        let queries = pool.client();

        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        queries.hold_read_snapshot(entered_tx, release_rx);
        entered_rx.recv().unwrap().unwrap();
        commit_revisions(&writer_client, 4);
        thread::sleep(Duration::from_millis(2));

        let blocked = writer_client.checkpoint().unwrap();
        assert!(blocked.busy || blocked.remaining_frames > 0, "{blocked:?}");
        assert!(queries.performance_snapshot().oldest_active_ns > 0);
        let blocked_metrics = writer_client.performance_snapshot().checkpoint;
        assert_eq!(blocked_metrics.attempts, 1);
        assert_eq!(blocked_metrics.blocked, 1);
        assert_eq!(blocked_metrics.completed, 0);
        assert!(blocked_metrics.last_remaining_frames > 0);
        assert!(blocked_metrics.blocked_by_reader_ns > 0);

        release_tx.send(()).unwrap();
        queries.overview().unwrap();
        let reclaimed = writer_client.checkpoint().unwrap();
        assert!(!reclaimed.busy, "{reclaimed:?}");
        assert_eq!(reclaimed.remaining_frames, 0);

        let mut wal_path = database.as_os_str().to_os_string();
        wal_path.push("-wal");
        let wal_bytes = std::fs::metadata(PathBuf::from(wal_path))
            .map(|metadata| metadata.len())
            .unwrap_or_default();
        assert_eq!(wal_bytes, 0, "completed TRUNCATE must reclaim the WAL");
        let recovered_metrics = writer_client.performance_snapshot().checkpoint;
        assert_eq!(recovered_metrics.attempts, 2);
        assert_eq!(recovered_metrics.completed, 1);
        assert_eq!(recovered_metrics.failures, 0);
        assert_eq!(recovered_metrics.last_remaining_frames, 0);
        assert!(recovered_metrics.blocked_by_reader_ns > 0);

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn cancellation_epoch_rejects_queued_usage_work() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("usage-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();

        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        client.hold_worker(entered_tx, release_rx);
        entered_rx.recv().unwrap();

        let queued_client = client.clone();
        let queued = thread::spawn(move || {
            queued_client.usage_activity(UsageActivityRequest {
                project_id: encode_entity_id(PROJECT_ID_PREFIX, b"project"),
                session_id: None,
                from: "2026-01-01".to_string(),
                to: "2026-12-31".to_string(),
            })
        });
        while client.commands.is_empty() {
            thread::yield_now();
        }
        client.cancel_pending();
        release_tx.send(()).unwrap();

        assert!(matches!(
            queued.join().unwrap(),
            Err(EngineError::QueryCancelled)
        ));

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn cancellation_epoch_rejects_queued_runtime_work() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("runtime-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();

        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        client.hold_worker(entered_tx, release_rx);
        entered_rx.recv().unwrap();

        let queued_client = client.clone();
        let queued = thread::spawn(move || {
            queued_client.runtime_snapshot(RuntimeSnapshotRequest {
                project_id: None,
                session_id: None,
                cursor: None,
                limit: DEFAULT_HISTORY_PAGE_LIMIT,
            })
        });
        while client.commands.is_empty() {
            thread::yield_now();
        }
        client.cancel_pending();
        release_tx.send(()).unwrap();

        assert!(matches!(
            queued.join().unwrap(),
            Err(EngineError::QueryCancelled)
        ));

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn pre_cancelled_team_request_never_enters_the_worker_queue() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("team-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();
        let cancellation = QueryCancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            client.teams_cancellable(
                TeamPageRequest {
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                cancellation,
            ),
            Err(EngineError::QueryCancelled)
        ));
        assert!(client.commands.is_empty());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn pre_cancelled_change_replay_never_enters_the_worker_queue() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("change-replay-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();
        let cancellation = QueryCancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            client.replay_changes_cancellable(
                ChangeReplayRequest {
                    after: None,
                    topics: Vec::new(),
                    limit: DEFAULT_CHANGE_REPLAY_LIMIT,
                },
                cancellation,
            ),
            Err(EngineError::QueryCancelled)
        ));
        assert!(client.commands.is_empty());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn pre_cancelled_capability_request_never_enters_the_worker_queue() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("capability-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();
        let cancellation = QueryCancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            client.plans_cancellable(
                PlanPageRequest {
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                cancellation,
            ),
            Err(EngineError::QueryCancelled)
        ));
        assert!(client.commands.is_empty());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn pre_cancelled_search_request_never_enters_the_worker_queue() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("search-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();
        let cancellation = QueryCancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            client.search_cancellable(
                SearchPageRequest {
                    text: "cancel me".to_string(),
                    project_id: None,
                    session_id: None,
                    adapter_ids: Vec::new(),
                    roles: Vec::new(),
                    native_kinds: Vec::new(),
                    branch_kind: None,
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                cancellation,
            ),
            Err(EngineError::QueryCancelled)
        ));
        assert!(client.commands.is_empty());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn pre_cancelled_timeline_request_never_enters_the_worker_queue() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("timeline-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();
        let cancellation = QueryCancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            client.timeline_cancellable(
                TimelinePageRequest {
                    project_id: encode_entity_id(PROJECT_ID_PREFIX, b"project"),
                    session_id: encode_entity_id(SESSION_ID_PREFIX, b"session"),
                    roles: Vec::new(),
                    native_kinds: Vec::new(),
                    include_content_kinds: Vec::new(),
                    include_tool_names: Vec::new(),
                    exclude_content_kinds: Vec::new(),
                    exclude_tool_names: Vec::new(),
                    search: None,
                    branch_kind: None,
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                cancellation,
            ),
            Err(EngineError::QueryCancelled)
        ));
        assert!(client.commands.is_empty());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn pre_cancelled_orchestration_request_never_enters_the_worker_queue() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("orchestration-cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();
        let cancellation = QueryCancellationToken::default();
        cancellation.cancel();

        assert!(matches!(
            client.workflows_cancellable(
                WorkflowPageRequest {
                    project_id: encode_entity_id(PROJECT_ID_PREFIX, b"project"),
                    session_id: encode_entity_id(SESSION_ID_PREFIX, b"session"),
                    cursor: None,
                    limit: DEFAULT_HISTORY_PAGE_LIMIT,
                },
                cancellation,
            ),
            Err(EngineError::QueryCancelled)
        ));
        assert!(client.commands.is_empty());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn request_cancellation_interrupts_running_sqlite_usage_work() {
        let connection = Connection::open_in_memory().unwrap();
        let control = Arc::new(QueryControl::new(1));
        let cancellation = QueryCancellationToken::default();
        let completed = Arc::new(AtomicBool::new(false));
        let query_completed = Arc::clone(&completed);

        let result = run_cancellable_query(&connection, &control, 0, &cancellation, || {
            // Cancellation happens after the preflight check and before this
            // deliberately expensive statement, so only SQLite's registered
            // progress handler can stop the running phase.
            cancellation.cancel();
            let value = connection
                .query_row(
                    r#"
                    WITH RECURSIVE values_under_test(value) AS (
                        SELECT 1
                        UNION ALL
                        SELECT value + 1 FROM values_under_test WHERE value < 1000000
                    )
                    SELECT SUM(value) FROM values_under_test
                    "#,
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| query_sqlite_error("run cancellable usage test", error))?;
            query_completed.store(true, Ordering::Release);
            Ok(value)
        });

        assert!(matches!(result, Err(EngineError::QueryCancelled)));
        assert!(
            !completed.load(Ordering::Acquire),
            "the recursive statement must be interrupted before completion"
        );
    }

    #[test]
    fn history_cursor_contract_rejects_cross_query_and_cross_project_reuse() {
        let project_cursor = HistoryCursorPayload {
            version: HISTORY_QUERY_CONTRACT_VERSION,
            kind: HistoryCursorKind::Projects,
            at_commit_seq: 7,
            sort_time: "2026-08-12T00:00:00Z".to_string(),
            entity_key: URL_SAFE_NO_PAD.encode(b"project"),
            project_id: None,
        };
        let encoded_project = encode_history_cursor(&project_cursor).unwrap();
        assert_eq!(
            decode_history_cursor(&encoded_project, HistoryCursorKind::Projects, None).unwrap(),
            project_cursor
        );
        assert!(matches!(
            decode_history_cursor(
                &encoded_project,
                HistoryCursorKind::Sessions,
                Some("project_v1_one")
            ),
            Err(EngineError::InvalidQuery(_))
        ));

        let session_cursor = HistoryCursorPayload {
            version: HISTORY_QUERY_CONTRACT_VERSION,
            kind: HistoryCursorKind::Sessions,
            at_commit_seq: 7,
            sort_time: String::new(),
            entity_key: URL_SAFE_NO_PAD.encode(b"session"),
            project_id: Some("project_v1_one".to_string()),
        };
        let encoded_session = encode_history_cursor(&session_cursor).unwrap();
        assert!(matches!(
            decode_history_cursor(
                &encoded_session,
                HistoryCursorKind::Sessions,
                Some("project_v1_two")
            ),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            decode_history_cursor("not-base64!", HistoryCursorKind::Projects, None),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            validate_history_page_limit(0),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            validate_history_page_limit(MAX_HISTORY_PAGE_LIMIT + 1),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(validate_history_cursor_watermark(Some(&project_cursor), 7).is_ok());
        assert!(matches!(
            validate_history_cursor_watermark(Some(&project_cursor), 8),
            Err(EngineError::InvalidQuery(_))
        ));
    }

    #[test]
    fn history_aggregation_uses_the_covering_message_activity_index() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("history-plan.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let connection = Connection::open(database).unwrap();
        let mut statement = connection
            .prepare(
                r#"
                EXPLAIN QUERY PLAN
                SELECT session_key, COUNT(*), MIN(source_time), MAX(source_time),
                       MAX(last_commit_seq)
                FROM canonical_messages
                GROUP BY session_key
                "#,
            )
            .unwrap();
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            details.iter().any(|detail| {
                detail.contains("COVERING INDEX idx_canonical_messages_session_activity")
            }),
            "history aggregation must not read message payload blobs: {details:?}"
        );

        writer.shutdown().unwrap();
    }

    #[test]
    fn durable_change_replay_is_ordered_filtered_paginated_and_watermarked() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("replay.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let receipt = writer
            .client()
            .commit_observation(commit_request())
            .unwrap();
        let mut pool = QueryPool::start(database, 2, None).unwrap();
        let client = pool.client();

        let first = client
            .replay_changes(ChangeReplayRequest {
                after: None,
                topics: Vec::new(),
                limit: 1,
            })
            .unwrap();
        assert_eq!(first.contract_version, CHANGE_REPLAY_CONTRACT_VERSION);
        assert_eq!(first.at_commit_seq, receipt.commit_seq);
        assert_eq!(
            first.oldest_available,
            Some(ChangeCursor {
                commit_seq: 1,
                ordinal: 0
            })
        );
        assert_eq!(first.changes.len(), 1);
        assert_eq!(first.changes[0].topic, "history.session.changed");
        assert_eq!(
            first.next_cursor,
            Some(ChangeCursor {
                commit_seq: 1,
                ordinal: 0
            })
        );
        assert!(first.has_more);
        assert_eq!(
            first.payload_bytes,
            change_payload_bytes(&first.changes[0]).unwrap()
        );
        assert_eq!(first.payload_byte_limit, MAX_CHANGE_REPLAY_PAYLOAD_BYTES);
        assert!(first.payload_bytes <= first.payload_byte_limit);

        let remainder = client
            .replay_changes(ChangeReplayRequest {
                after: first.next_cursor,
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(remainder.changes.len(), 1);
        assert_eq!(remainder.changes[0].cursor.ordinal, 1);
        assert!(!remainder.has_more);

        let runtime_only = client
            .replay_changes(ChangeReplayRequest {
                after: None,
                topics: vec!["runtime.session.changed".to_string()],
                limit: 10,
            })
            .unwrap();
        assert_eq!(runtime_only.changes.len(), 1);
        assert_eq!(runtime_only.changes[0].payload, b"runtime");

        let after_snapshot = client
            .replay_changes(ChangeReplayRequest {
                after: Some(ChangeCursor::after_snapshot(receipt.commit_seq)),
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap();
        assert!(after_snapshot.changes.is_empty());

        assert!(matches!(
            client.replay_changes(ChangeReplayRequest {
                after: None,
                topics: Vec::new(),
                limit: 0,
            }),
            Err(EngineError::InvalidQuery(_))
        ));
        assert!(matches!(
            client.replay_changes(ChangeReplayRequest {
                after: None,
                topics: vec!["history.session.changed".to_string(); 65],
                limit: 10,
            }),
            Err(EngineError::InvalidQuery(_))
        ));

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn durable_change_replay_splits_pages_at_the_payload_byte_limit() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("replay-payload-bound.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut request = commit_request();
        let payload_len = usize::try_from(MAX_CHANGE_REPLAY_PAYLOAD_BYTES / 2).unwrap();
        for (index, change) in request.changes.iter_mut().enumerate() {
            change.payload = vec![u8::try_from(index).unwrap(); payload_len];
        }
        writer.client().commit_observation(request).unwrap();
        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let client = pool.client();

        let first = client
            .replay_changes(ChangeReplayRequest {
                after: None,
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(first.changes.len(), 1);
        assert!(first.has_more);
        assert!(first.payload_bytes <= MAX_CHANGE_REPLAY_PAYLOAD_BYTES);

        let second = client
            .replay_changes(ChangeReplayRequest {
                after: first.next_cursor,
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(second.changes.len(), 1);
        assert!(!second.has_more);
        assert!(second.payload_bytes <= MAX_CHANGE_REPLAY_PAYLOAD_BYTES);

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn retention_prunes_complete_commits_and_requires_stale_cursor_reset() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("replay-retention.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let client = writer.client();
        commit_revisions(&client, 4);
        let writer_performance = client.performance_snapshot();
        assert_eq!(writer_performance.commit_attempts, 4);
        assert_eq!(writer_performance.committed, 4);
        assert_eq!(writer_performance.failed, 0);
        assert_eq!(writer_performance.facts_committed, 4);
        assert!(writer_performance.sqlite_rows_changed > 0);
        assert_eq!(
            writer_performance
                .timings
                .iter()
                .find(|timing| timing.name == "writer_total")
                .unwrap()
                .latency
                .samples,
            4,
        );

        let retention = client
            .maintain_change_log(
                ChangeLogRetentionPolicy {
                    // Keep every fixture commit by age so this exercises the
                    // independent logical-size boundary.
                    max_age_ms: 10_000,
                    max_payload_bytes: 1,
                    min_resumable_commits: 2,
                },
                1_000,
            )
            .unwrap();
        assert_eq!(retention.pruned_through_commit_seq, 2);
        assert_eq!(retention.retained_change_count, 4);
        assert!(retention.retained_payload_bytes > 0);
        assert_eq!(retention.oldest_retained_commit_seq, Some(3));
        assert_eq!(retention.oldest_retained_ordinal, Some(0));

        let mut pool = QueryPool::start(database, 1, None).unwrap();
        let queries = pool.client();
        let stale = queries
            .replay_changes(ChangeReplayRequest {
                after: Some(ChangeCursor {
                    commit_seq: 2,
                    ordinal: u32::MAX,
                }),
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap_err();
        assert!(matches!(
            stale,
            EngineError::ResetRequired {
                current_commit_seq: 4,
                oldest_commit_seq: Some(3),
                oldest_ordinal: Some(0),
            }
        ));

        let retained = queries
            .replay_changes(ChangeReplayRequest {
                after: Some(ChangeCursor {
                    commit_seq: 3,
                    ordinal: 0,
                }),
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(retained.changes.len(), 3);
        assert_eq!(retained.oldest_available.unwrap().commit_seq, 3);

        let overview = queries.overview().unwrap();
        assert_eq!(overview.change_log_pruned_through_seq, 2);
        assert_eq!(overview.change_log_retained_changes, 4);
        assert_eq!(
            overview.change_log_oldest_cursor,
            Some(ChangeCursor {
                commit_seq: 3,
                ordinal: 0,
            })
        );

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn retention_age_boundary_is_independent_of_the_size_target() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("replay-retention-age.db");
        let mut writer = WriterRuntime::start(database).unwrap();
        let client = writer.client();
        commit_revisions(&client, 4);

        let retention = client
            .maintain_change_log(
                ChangeLogRetentionPolicy {
                    max_age_ms: 25,
                    max_payload_bytes: 1024 * 1024,
                    min_resumable_commits: 2,
                },
                46,
            )
            .unwrap();
        // cutoff=21 includes commits 1 and 2, while the two-commit minimum
        // independently protects commits 3 and 4.
        assert_eq!(retention.pruned_through_commit_seq, 2);
        assert_eq!(retention.oldest_retained_commit_seq, Some(3));
        assert_eq!(retention.retained_change_count, 4);

        writer.shutdown().unwrap();
    }

    #[test]
    fn restart_replays_changes_committed_before_in_memory_publication() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("restart-replay.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        writer
            .client()
            .commit_observation(commit_request())
            .unwrap();
        writer.shutdown().unwrap();

        let mut restarted_writer = WriterRuntime::start(database.clone()).unwrap();
        let mut restarted_queries = QueryPool::start(database, 1, None).unwrap();
        let replay = restarted_queries
            .client()
            .replay_changes(ChangeReplayRequest {
                after: None,
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(replay.at_commit_seq, 1);
        assert_eq!(replay.changes.len(), 2);
        assert_eq!(
            replay.changes[0].cursor,
            ChangeCursor {
                commit_seq: 1,
                ordinal: 0
            }
        );
        assert_eq!(
            replay.changes[1].cursor,
            ChangeCursor {
                commit_seq: 1,
                ordinal: 1
            }
        );

        restarted_queries.shutdown().unwrap();
        restarted_writer.shutdown().unwrap();
    }

    #[test]
    fn catalog_snapshot_hydrates_driver_and_decoder_state_after_restart() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("catalog-restart.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut request = commit_request();
        request.object.driver_checkpoint = Some(b"driver-v1".to_vec());
        request.object.driver_checkpoint_version = Some(1);
        request.object.decoder_state = Some(b"adapter-v7".to_vec());
        request.object.decoder_state_version = Some(7);
        let receipt = writer.client().commit_observation(request).unwrap();
        writer.shutdown().unwrap();

        let mut restarted_queries = QueryPool::start(database, 1, None).unwrap();
        let missing = restarted_queries
            .client()
            .source_catalog("query-fixture", b"missing")
            .unwrap();
        assert_eq!(missing.source_instance_id, None);
        assert!(missing.streams.is_empty() && missing.objects.is_empty());

        let catalog = restarted_queries
            .client()
            .source_catalog("query-fixture", b"root")
            .unwrap();
        assert_eq!(catalog.source_instance_id, Some(receipt.source_instance_id));
        assert_eq!(catalog.adapter_contract_version, Some(1));
        assert_eq!(catalog.streams.len(), 1);
        assert_eq!(
            catalog.streams[0].source_stream_id,
            receipt.source_stream_id
        );
        assert_eq!(catalog.objects.len(), 1);
        let object = &catalog.objects[0];
        assert_eq!(object.source_object_id, receipt.source_object_id);
        assert_eq!(object.committed_cursor, b"cursor");
        assert_eq!(
            object.driver_checkpoint.as_deref(),
            Some(b"driver-v1".as_slice())
        );
        assert_eq!(object.driver_checkpoint_version, Some(1));
        assert_eq!(
            object.decoder_state.as_deref(),
            Some(b"adapter-v7".as_slice())
        );
        assert_eq!(object.decoder_state_version, Some(7));
        assert_eq!(object.last_commit_seq, Some(receipt.commit_seq));
        assert_eq!(object.state, "active");

        assert!(matches!(
            restarted_queries.client().source_catalog("", b"root"),
            Err(EngineError::InvalidQuery(_))
        ));
        restarted_queries.shutdown().unwrap();
    }
}
