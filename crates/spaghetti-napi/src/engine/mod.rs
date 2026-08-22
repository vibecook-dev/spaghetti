//! Persistent observation/query engine lifecycle (RFC 011).
//!
//! This module is deliberately free of N-API types. The Node binding in
//! `napi_engine` is one host adapter; a future daemon or embedded desktop host
//! can own the same `SpaghettiEngineCore` and receive identical semantics.

mod artifact_projection;
mod capability_query;
mod catalog_publication;
mod catalog_query;
mod catalog_retention;
mod catalog_state;
mod commit;
mod coordinator;
mod coverage_query;
mod detail_query;
mod ingest_profile;
mod local_permissions;
mod memory_projection;
mod observation;
mod orchestration_query;
mod owner_lock;
mod performance;
mod presence_projection;
mod projection;
mod query_identity;
mod query_pool;
mod runtime_query;
mod runtime_semantic_merge;
mod runtime_semantic_projection;
mod runtime_usage_query;
mod runtime_usage_totals_query;
mod search_query;
mod session_index_projection;
mod settings_projection;
mod source_coverage;
mod storage_codec;
mod supervisor;
mod task_projection;
mod team_projection;
mod team_query;
mod timeline_projection;
mod timeline_query;
mod tool_result_projection;
mod unknown_evidence_projection;
mod usage_query;
mod workflow_projection;
mod writer;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

use crate::adapter::{
    AdapterId, AdapterRegistry, FactBatch, SourceCoverageSet, TypedAccessAuthorization,
};
pub use capability_query::{
    ArtifactDetail, ArtifactPage, ArtifactPageRequest, MemoryDocument, MemoryDocumentPage,
    MemoryDocumentPageRequest, PlanDetail, PlanPage, PlanPageRequest, TaskCollectionPage,
    TaskCollectionPageRequest, TaskCollectionSummary, TaskDetail, TaskPage, TaskPageRequest,
    ToolResultDetail, ToolResultPage, ToolResultPageRequest, CAPABILITY_QUERY_CONTRACT_VERSION,
    DEFAULT_CAPABILITY_PAGE_LIMIT, MAX_CAPABILITY_PAGE_PAYLOAD_BYTES,
};
use catalog_publication::{
    CatalogInitialPublicationCommand, CatalogInitialPublicationReceipt,
    CatalogRefreshPublicationCommand, CatalogRefreshPublicationReceipt,
};
pub(crate) use catalog_query::{
    CatalogPageQueryRequest, CatalogResolutionQueryRequest, CatalogRetainedPageOutcome,
};
use catalog_retention::{CatalogSnapshotRetirementCommand, CatalogSnapshotRetirementReceipt};
use catalog_state::CatalogBuildStateCommand;
pub use commit::{
    ChangeLogRetentionPolicy, ChangeLogRetentionSnapshot, DEFAULT_CHANGE_LOG_MAX_AGE_MS,
    DEFAULT_CHANGE_LOG_MAX_PAYLOAD_BYTES, DEFAULT_CHANGE_LOG_MIN_RESUMABLE_COMMITS,
};
use commit::{
    CommitReceipt, ObservationCommit, ProjectionVersionCommit, QueryPackProjectionGuard,
    QueryPackSelectionExpectation, QueryPackSelectionUpdate, QueryPackSelectionValue,
};
pub use coordinator::{
    FactFamilyReplayRequest, ObservationCoordinator, ReconcileOutcome, ReconcileRequest,
    ReconcileRetryTarget,
};
pub use coverage_query::{
    FactFamilyCoverageItem, FactFamilyCoveragePage, FactFamilyCoveragePageRequest,
    FactFamilyCoverageSetSummary, DEFAULT_FACT_FAMILY_COVERAGE_PAGE_LIMIT,
    FACT_FAMILY_COVERAGE_QUERY_CONTRACT_VERSION, MAX_FACT_FAMILY_COVERAGE_PAGE_LIMIT,
};
pub use detail_query::{
    CanonicalStats, MessageDetail, MessagePage, MessagePageRequest, NamedCount, SessionDetail,
    SessionDetails, SessionDetailsRequest, SessionIndexDetail, SourceCapabilitySummary, SourcePage,
    SourcePageRequest, SourceSummary, DEFAULT_DETAIL_PAGE_LIMIT, DETAIL_QUERY_CONTRACT_VERSION,
    MAX_MESSAGE_PAGE_PAYLOAD_BYTES,
};
pub use observation::ObservationStatusSnapshot;
use observation::{ObservationLease, ObservationRuntime, PendingObservationWork};
pub use orchestration_query::{
    DelegationPage, DelegationPageRequest, DelegationSummary, WorkflowDetails,
    WorkflowDetailsRequest, WorkflowMember, WorkflowMemberPage, WorkflowMemberPageRequest,
    WorkflowPage, WorkflowPageRequest, WorkflowSummary, DEFAULT_ORCHESTRATION_PAGE_LIMIT,
    MAX_ORCHESTRATION_PAGE_PAYLOAD_BYTES, ORCHESTRATION_QUERY_CONTRACT_VERSION,
};
use owner_lock::DatabaseOwnerLock;
pub use owner_lock::OwnerMetadata;
pub use performance::{
    CheckpointPerformanceSnapshot, EnginePerformanceSnapshot, LatencySnapshot,
    NamedLatencySnapshot, QueryPerformanceSnapshot, RuntimeUsageCompatibilityTelemetrySnapshot,
    SourceDimensionPerformanceSnapshot, SourcePerformanceSnapshot, SourcePipelineSnapshot,
    StoragePerformanceSnapshot, WriterPerformanceSnapshot,
};
use performance::{SourcePerformanceRecorder, SourceTelemetry};
pub use query_pool::{
    ChangeCursor, ChangeReplay, ChangeReplayRequest, DurableChange, HistoryProjectIndexSummary,
    HistoryProjectPage, HistoryProjectPageRequest, HistoryProjectSummary,
    HistorySessionIndexSummary, HistorySessionPage, HistorySessionPageRequest,
    HistorySessionSummary, QueryCancellationToken, QueryOverview, CHANGE_REPLAY_CONTRACT_VERSION,
    DEFAULT_CHANGE_REPLAY_LIMIT, DEFAULT_HISTORY_PAGE_LIMIT, HISTORY_QUERY_CONTRACT_VERSION,
    MAX_CHANGE_REPLAY_PAYLOAD_BYTES,
};
use query_pool::{QueryClient, QueryPool, SourceCatalogSnapshot, SourceCoverageReplayBaseline};
pub use runtime_query::{
    RunStateLookup, RunStateRequest, RuntimePresenceSnapshot, RuntimeRunEvidence,
    RuntimeRunSnapshot, RuntimeSnapshot, RuntimeSnapshotEntry, RuntimeSnapshotRequest,
    DEFAULT_RUNTIME_PAGE_LIMIT, RUNTIME_QUERY_CONTRACT_VERSION,
};
pub use runtime_usage_query::{
    RuntimeUsageQuerySelection, RuntimeUsageQuerySelectionValue, RuntimeUsageV2ActorContext,
    RuntimeUsageV2Affiliation, RuntimeUsageV2Aggregate, RuntimeUsageV2BucketAggregate,
    RuntimeUsageV2ExternalEntityRef, RuntimeUsageV2Page, RuntimeUsageV2PageRequest,
    RuntimeUsageV2ProjectionReadiness, RuntimeUsageV2Response, RuntimeUsageV2SemanticRevisionRef,
    RuntimeUsageV2TextValue, RuntimeUsageV2TokenValue, RuntimeUsageV2ValueProvenance,
    DEFAULT_RUNTIME_USAGE_V2_PAGE_LIMIT, MAX_RUNTIME_USAGE_V2_PAGE_LIMIT,
    RUNTIME_USAGE_QUERY_SELECTION_CONTRACT_VERSION, RUNTIME_USAGE_V2_QUERY_CONTRACT_VERSION,
};
pub use runtime_usage_totals_query::{
    RuntimeUsageCompatibilityBucket, RuntimeUsageCompatibilityReport,
    RuntimeUsageCompatibilityRequest, RuntimeUsageLegacyTotals, RuntimeUsageTotalsReport,
    RuntimeUsageTotalsRequest, RuntimeUsageTotalsSelectionScope, MAX_RUNTIME_USAGE_TOTALS_SCOPES,
    RUNTIME_USAGE_COMPATIBILITY_QUERY_CONTRACT_VERSION,
    RUNTIME_USAGE_TOTALS_QUERY_CONTRACT_VERSION, SELECTED_RUNTIME_USAGE_QUERY_ID,
};
pub use search_query::{
    SearchHit, SearchPage, SearchPageRequest, DEFAULT_SEARCH_PAGE_LIMIT,
    MAX_SEARCH_PAGE_PAYLOAD_BYTES, SEARCH_QUERY_CONTRACT_VERSION,
};
use supervisor::ObservationSupervisor;
pub use supervisor::ObservationSupervisorOptions;
pub use team_query::{
    TeamConfigSummary, TeamDetails, TeamDetailsRequest, TeamInboxMessage, TeamInboxMessagePage,
    TeamInboxMessagePageRequest, TeamInboxPage, TeamInboxPageRequest, TeamInboxSummary, TeamMember,
    TeamPage, TeamPageRequest, TeamSummary, DEFAULT_TEAM_PAGE_LIMIT, TEAM_QUERY_CONTRACT_VERSION,
};
pub use timeline_query::{
    TimelineFacets, TimelineMessage, TimelinePage, TimelinePageRequest,
    DEFAULT_TIMELINE_PAGE_LIMIT, MAX_TIMELINE_PAGE_PAYLOAD_BYTES, TIMELINE_QUERY_CONTRACT_VERSION,
};
pub use usage_query::{
    UntimedUsageSummary, UsageActivityDay, UsageActivityReport, UsageActivityRequest,
    UsageAggregate, UsageCoverageSummary, UsageScopeRequest, UsageTokenValues, UsageTotalsReport,
    MAX_USAGE_ACTIVITY_DAYS, USAGE_QUERY_CONTRACT_VERSION,
};
use writer::{WriterClient, WriterRuntime};

const DEFAULT_QUERY_WORKERS: usize = 2;
const MAX_QUERY_WORKERS: usize = 16;
pub const FACT_FAMILY_REPLAY_COMMAND_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_COMMIT_WAIT_TIMEOUT_MS: u32 = 30_000;
pub const MAX_COMMIT_WAIT_TIMEOUT_MS: u32 = 300_000;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("invalid engine configuration: {0}")]
    InvalidConfig(String),

    #[error(
        "database is already owned: {database_path}; lock={lock_path}; current_owner={owner:?}"
    )]
    OwnerBusy {
        database_path: PathBuf,
        lock_path: PathBuf,
        owner: Option<Box<OwnerMetadata>>,
    },

    #[error("database owner lock failed at {lock_path}: {detail}")]
    OwnerLock { lock_path: PathBuf, detail: String },

    #[error("could not start {worker} worker: {detail}")]
    WorkerStart {
        worker: &'static str,
        detail: String,
    },

    #[error("{worker} worker is unavailable")]
    WorkerUnavailable { worker: &'static str },

    #[error("{worker} worker panicked during shutdown")]
    WorkerPanic { worker: &'static str },

    #[error("SQLite {operation} failed: {detail}")]
    Sqlite {
        operation: &'static str,
        detail: String,
    },

    #[error("storage codec {operation} failed: {detail}")]
    StorageCodec {
        operation: &'static str,
        detail: String,
    },

    #[error(
        "insufficient disk space for observation database at {database_path}: available={available_bytes} bytes, required reserve={reserve_bytes} bytes"
    )]
    InsufficientDiskSpace {
        database_path: PathBuf,
        available_bytes: u64,
        reserve_bytes: u64,
    },

    #[error("engine is shutting down or already stopped")]
    ShuttingDown,

    #[error("search is unavailable until query bootstrap completes")]
    BootstrapInProgress,

    #[error("query was cancelled")]
    QueryCancelled,

    #[error("query queue is full")]
    QueryQueueFull,

    #[error(
        "RESET_REQUIRED current_commit_seq={current_commit_seq} oldest_commit_seq={oldest_commit_seq:?} oldest_ordinal={oldest_ordinal:?}"
    )]
    ResetRequired {
        current_commit_seq: u64,
        oldest_commit_seq: Option<u64>,
        oldest_ordinal: Option<u32>,
    },

    #[error("invalid query: {0}")]
    InvalidQuery(String),

    #[error("invalid observation commit: {0}")]
    InvalidCommit(String),

    #[error("observation coordinator {operation} failed: {detail}")]
    Observation {
        operation: &'static str,
        detail: String,
    },

    #[error("observation reconcile is already in progress; the requested scope was marked dirty")]
    ObservationBusy,

    #[error("source cursor changed before commit for adapter {adapter_id}, stream {stream_key}")]
    StaleSourceCursor {
        adapter_id: String,
        stream_key: String,
    },

    #[error("injected process failure at {stage}")]
    InjectedFailure { stage: &'static str },
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub database_path: PathBuf,
    pub query_workers: Option<usize>,
    pub owner_label: Option<String>,
    pub defer_query_structures: bool,
    /// Caller-owned fair permit domain shared with observer source passes.
    pub(crate) source_pass_pool: Option<crate::source::SharedSourcePassPool>,
}

/// Explicit administrative command that replaces one fact family's durable
/// evidence for a source instance selected through an opaque project/session
/// scope. The three expected coverage fields form a stale-safe authorization
/// copied from `getFactFamilyCoverage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFamilyReplayCommand {
    pub adapter_id: String,
    pub configured_roots: Vec<PathBuf>,
    pub project_id: String,
    pub session_id: String,
    pub owner_id: String,
    pub family: String,
    pub family_version: u32,
    pub expected_source_instance_ref: String,
    pub expected_content_digest_ref: String,
    pub expected_coverage_last_commit_seq: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFamilyReplayResult {
    pub contract_version: u32,
    pub project_id: String,
    pub session_id: String,
    pub owner_id: String,
    pub family: String,
    pub family_version: u32,
    pub authorized_source_instance_ref: String,
    pub authorized_content_digest_ref: String,
    pub authorized_coverage_last_commit_seq: u64,
    pub outcome: ReconcileOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageQuerySelectionCommand {
    pub project_id: String,
    pub session_id: String,
    pub target_query_id: String,
    pub expected_materialized: bool,
    pub expected_selected_query_id: String,
    pub expected_selected_contract_version: u32,
    pub expected_selection_epoch: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUsageQuerySelectionResult {
    pub contract_version: u32,
    pub at_commit_seq: u64,
    pub project_id: String,
    pub session_id: String,
    pub selection: RuntimeUsageQuerySelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecyclePhase {
    Running,
    Stopping,
    Stopped,
}

impl LifecyclePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}

struct EngineRuntime {
    owner_lock: DatabaseOwnerLock,
    writer: WriterRuntime,
    queries: Option<QueryPool>,
    bootstrap_active: bool,
}

struct Lifecycle {
    phase: LifecyclePhase,
    runtime: Option<EngineRuntime>,
}

#[derive(Debug, Clone)]
pub struct EngineStatusSnapshot {
    pub state: String,
    pub database_path: String,
    pub accepting_queries: bool,
    pub catalog_query_ready: bool,
    pub search_available: bool,
    pub writer_alive: bool,
    pub configured_query_workers: u32,
    pub alive_query_workers: u32,
    pub in_flight_queries: u32,
    pub observation: ObservationStatusSnapshot,
    pub owner: Option<OwnerMetadata>,
}

/// RFC 012B catalog-first host lifecycle: last-complete catalog may be
/// queryable while FTS/query bootstrap is still incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressiveHostReadiness {
    pub catalog_query_ready: bool,
    pub search_available: bool,
    pub selected_hydration_available: bool,
    pub bootstrap_active: bool,
}

#[derive(Debug, Clone)]
pub struct EngineHealthSnapshot {
    pub status: EngineStatusSnapshot,
    pub healthy: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EngineOverview {
    pub schema_version: u32,
    pub commit_seq: u64,
    /// Transitional compatibility-table counts.
    pub projects: u32,
    pub sessions: u32,
    pub messages: u32,
    /// Canonical history materialized by RFC 011 observation commits.
    pub canonical_sessions: u32,
    pub canonical_messages: u32,
    pub change_log_oldest_cursor: Option<ChangeCursor>,
    pub change_log_pruned_through_seq: u64,
    pub change_log_retained_changes: u64,
    pub change_log_retained_payload_bytes: u64,
    pub writer_data_version: u32,
    pub journal_mode: String,
    pub query_only: bool,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitWaitResult {
    pub observed_commit_seq: u64,
    pub reason: String,
    pub waited_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommitNotificationState {
    latest_commit_seq: u64,
    stopping: bool,
}

#[derive(Debug)]
struct CommitNotifications {
    changed: tokio::sync::watch::Sender<CommitNotificationState>,
}

impl CommitNotifications {
    fn new(latest_commit_seq: u64) -> Self {
        let (changed, _) = tokio::sync::watch::channel(CommitNotificationState {
            latest_commit_seq,
            stopping: false,
        });
        Self { changed }
    }

    fn publish(&self, commit_seq: u64) {
        self.changed.send_if_modified(|state| {
            if commit_seq <= state.latest_commit_seq {
                return false;
            }
            state.latest_commit_seq = commit_seq;
            true
        });
    }

    fn latest_commit_seq(&self) -> u64 {
        self.changed.borrow().latest_commit_seq
    }

    fn stop(&self) {
        self.changed.send_if_modified(|state| {
            if state.stopping {
                return false;
            }
            state.stopping = true;
            true
        });
    }

    async fn wait(
        &self,
        after_commit_seq: u64,
        timeout_ms: u32,
        cancellation: &QueryCancellationToken,
    ) -> Result<CommitWaitResult, EngineError> {
        if timeout_ms == 0 || timeout_ms > MAX_COMMIT_WAIT_TIMEOUT_MS {
            return Err(EngineError::InvalidQuery(format!(
                "commit wait timeout_ms must be between 1 and {MAX_COMMIT_WAIT_TIMEOUT_MS}, got {timeout_ms}"
            )));
        }
        let started = Instant::now();
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut changed = self.changed.subscribe();

        loop {
            if cancellation.is_cancelled() {
                return Err(EngineError::QueryCancelled);
            }
            let state = *changed.borrow_and_update();
            if state.stopping {
                return Err(EngineError::ShuttingDown);
            }
            if state.latest_commit_seq > after_commit_seq {
                return Ok(CommitWaitResult {
                    observed_commit_seq: state.latest_commit_seq,
                    reason: "commit".to_string(),
                    waited_ms: duration_ms(started.elapsed()),
                });
            }

            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(EngineError::QueryCancelled),
                result = changed.changed() => {
                    if result.is_err() {
                        return Err(EngineError::ShuttingDown);
                    }
                }
                _ = &mut deadline => {
                    let state = *changed.borrow();
                    if state.stopping {
                        return Err(EngineError::ShuttingDown);
                    }
                    return Ok(CommitWaitResult {
                        observed_commit_seq: state.latest_commit_seq,
                        reason: if state.latest_commit_seq > after_commit_seq {
                            "commit"
                        } else {
                            "timeout"
                        }
                        .to_string(),
                        waited_ms: duration_ms(started.elapsed()),
                    });
                }
            }
        }
    }
}

/// One persistent engine owner for one canonical database.
pub struct SpaghettiEngineCore {
    database_path: PathBuf,
    owner: OwnerMetadata,
    query_workers: usize,
    adapters: Arc<AdapterRegistry>,
    observation: Arc<ObservationRuntime>,
    source_telemetry: Arc<SourceTelemetry>,
    observation_workers: Mutex<Option<Arc<rayon::ThreadPool>>>,
    supervisors: Mutex<Vec<ObservationSupervisor>>,
    lifecycle: Mutex<Lifecycle>,
    commit_notifications: CommitNotifications,
    stopped: Condvar,
    source_pass_pool: Option<crate::source::SharedSourcePassPool>,
}

impl SpaghettiEngineCore {
    pub fn open(options: EngineOptions) -> Result<Arc<Self>, EngineError> {
        Self::open_with_registry(
            options,
            AdapterRegistry::builder()
                .build()
                .map_err(|error| EngineError::InvalidConfig(error.to_string()))?,
        )
    }

    pub fn open_with_registry(
        options: EngineOptions,
        adapters: AdapterRegistry,
    ) -> Result<Arc<Self>, EngineError> {
        let database_path = normalize_database_path(&options.database_path)?;
        let query_workers = options.query_workers.unwrap_or(DEFAULT_QUERY_WORKERS);
        if !(1..=MAX_QUERY_WORKERS).contains(&query_workers) {
            return Err(EngineError::InvalidConfig(format!(
                "query_workers must be between 1 and {MAX_QUERY_WORKERS}, got {query_workers}"
            )));
        }

        let owner_label = options
            .owner_label
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "napi".to_string());
        let owner_lock = DatabaseOwnerLock::acquire(&database_path, owner_label)?;
        let owner = owner_lock.metadata().clone();
        let writer = WriterRuntime::start(database_path.clone())?;
        let bootstrap_active =
            options.defer_query_structures && writer.client().begin_query_bootstrap()?;
        // Catalog-first: admit the read pool while FTS bootstrap is still
        // incomplete. Search stays BootstrapInProgress until finalization.
        let source_pass_pool = options.source_pass_pool.clone();
        let queries = Some(QueryPool::start(
            database_path.clone(),
            query_workers,
            source_pass_pool.clone(),
        )?);
        let latest_commit_seq = queries
            .as_ref()
            .map(|pool| pool.client().overview().map(|overview| overview.commit_seq))
            .transpose()?
            .unwrap_or_default();
        let observation_workers = rayon::ThreadPoolBuilder::new()
            .num_threads(16)
            .thread_name(|index| format!("spaghetti-observe-{index}"))
            .build()
            .map_err(|error| EngineError::WorkerStart {
                worker: "observation decode pool",
                detail: error.to_string(),
            })?;

        Ok(Arc::new(Self {
            database_path,
            owner,
            query_workers,
            adapters: Arc::new(adapters),
            observation: ObservationRuntime::new(),
            source_telemetry: SourceTelemetry::new(),
            observation_workers: Mutex::new(Some(Arc::new(observation_workers))),
            supervisors: Mutex::new(Vec::new()),
            lifecycle: Mutex::new(Lifecycle {
                phase: LifecyclePhase::Running,
                runtime: Some(EngineRuntime {
                    owner_lock,
                    writer,
                    queries,
                    bootstrap_active,
                }),
            }),
            commit_notifications: CommitNotifications::new(latest_commit_seq),
            stopped: Condvar::new(),
            source_pass_pool,
        }))
    }

    pub fn status(&self) -> EngineStatusSnapshot {
        let lifecycle = self.lock_lifecycle();
        let (writer_alive, alive_query_workers, in_flight_queries, bootstrap_active) = lifecycle
            .runtime
            .as_ref()
            .map(|runtime| {
                let writer = runtime.writer.client();
                let queries = runtime.queries.as_ref().map(QueryPool::client);
                (
                    writer.is_alive(),
                    queries
                        .as_ref()
                        .map(QueryClient::alive_workers)
                        .unwrap_or_default(),
                    queries
                        .as_ref()
                        .map(QueryClient::in_flight)
                        .unwrap_or_default(),
                    runtime.bootstrap_active,
                )
            })
            .unwrap_or((false, 0, 0, false));
        let running = lifecycle.phase == LifecyclePhase::Running;
        let mut observation = self.observation.snapshot();
        let latest_commit_seq = self.commit_notifications.latest_commit_seq();
        if latest_commit_seq > 0 {
            observation.last_commit_seq = Some(
                observation
                    .last_commit_seq
                    .unwrap_or_default()
                    .max(latest_commit_seq),
            );
        }
        let supervisors = self.lock_supervisors();
        for supervisor in supervisors
            .iter()
            .filter(|supervisor| supervisor.is_alive())
        {
            observation.supervisors_running = observation.supervisors_running.saturating_add(1);
            observation.watched_instances = observation
                .watched_instances
                .saturating_add(supervisor.watched_instances());
            observation.watch_roots = observation
                .watch_roots
                .saturating_add(supervisor.watch_roots());
            if !supervisor.watcher_available() {
                observation.state = "degraded".to_string();
                observation.recovery_required = true;
                observation.last_error.get_or_insert_with(|| {
                    "native watcher unavailable; observation is using polling fallback".to_string()
                });
            }
        }

        let catalog_query_ready = running && self.last_complete_catalog_readable();
        let search_available = running && !bootstrap_active && catalog_query_ready;
        EngineStatusSnapshot {
            state: if running && bootstrap_active {
                "bootstrapping".to_string()
            } else {
                lifecycle.phase.as_str().to_string()
            },
            database_path: self.database_path.to_string_lossy().into_owned(),
            accepting_queries: running && alive_query_workers > 0,
            catalog_query_ready,
            search_available,
            writer_alive,
            configured_query_workers: usize_to_u32(self.query_workers),
            alive_query_workers: usize_to_u32(alive_query_workers),
            in_flight_queries: usize_to_u32(in_flight_queries),
            observation,
            owner: (lifecycle.phase != LifecyclePhase::Stopped).then(|| self.owner.clone()),
        }
    }

    /// Catalog-first readiness used by the observation host.
    pub fn progressive_host_readiness(&self) -> ProgressiveHostReadiness {
        let status = self.status();
        let bootstrap_active = status.state == "bootstrapping";
        ProgressiveHostReadiness {
            catalog_query_ready: status.catalog_query_ready,
            search_available: status.search_available,
            selected_hydration_available: status.catalog_query_ready,
            bootstrap_active,
        }
    }

    fn last_complete_catalog_readable(&self) -> bool {
        let Ok(connection) =
            Connection::open_with_flags(&self.database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            return false;
        };
        match catalog_state::load_catalog_build_state(&connection) {
            Ok(Some(state)) => state.ready_read_authority().is_ok(),
            _ => false,
        }
    }

    pub fn health(&self) -> EngineHealthSnapshot {
        let initial = self.status();
        if !initial.accepting_queries {
            return EngineHealthSnapshot {
                healthy: false,
                detail: Some(format!("engine is {}", initial.state)),
                status: initial,
            };
        }

        let result = self.clients().and_then(|(writer, queries)| {
            let writer_health = writer.health()?;
            let query_health = queries.overview()?;
            if !query_health.query_only || !query_health.read_only {
                return Err(EngineError::Sqlite {
                    operation: "verify query confinement",
                    detail: "query connection is not read-only/query-only".to_string(),
                });
            }
            if writer_health.journal_mode.to_lowercase() != "wal" {
                return Err(EngineError::Sqlite {
                    operation: "verify writer journal mode",
                    detail: format!("expected WAL, got {}", writer_health.journal_mode),
                });
            }
            Ok(())
        });
        let status = self.status();
        let worker_counts_healthy =
            status.writer_alive && status.alive_query_workers == status.configured_query_workers;

        match result {
            Ok(()) if !worker_counts_healthy => EngineHealthSnapshot {
                status,
                healthy: false,
                detail: Some("one or more persistent workers are unavailable".to_string()),
            },
            Err(error) => EngineHealthSnapshot {
                status,
                healthy: false,
                detail: Some(error.to_string()),
            },
            Ok(()) if status.observation.recovery_required => EngineHealthSnapshot {
                detail: status
                    .observation
                    .last_error
                    .clone()
                    .or_else(|| Some("observation reconcile requires recovery".to_string())),
                status,
                healthy: false,
            },
            Ok(()) => EngineHealthSnapshot {
                status,
                healthy: true,
                detail: None,
            },
        }
    }

    pub fn overview(&self) -> Result<EngineOverview, EngineError> {
        let (writer, queries) = self.clients()?;
        let writer_health = writer.health()?;
        let query = queries.overview()?;
        Ok(EngineOverview {
            schema_version: query.schema_version,
            commit_seq: query.commit_seq,
            projects: query.projects,
            sessions: query.sessions,
            messages: query.messages,
            canonical_sessions: query.canonical_sessions,
            canonical_messages: query.canonical_messages,
            change_log_oldest_cursor: query.change_log_oldest_cursor,
            change_log_pruned_through_seq: query.change_log_pruned_through_seq,
            change_log_retained_changes: query.change_log_retained_changes,
            change_log_retained_payload_bytes: query.change_log_retained_payload_bytes,
            writer_data_version: writer_health.data_version,
            journal_mode: writer_health.journal_mode,
            query_only: query.query_only,
            read_only: query.read_only,
        })
    }

    /// List canonical projects through one bounded, snapshot-consistent
    /// read-only query operation.
    pub fn history_projects(
        &self,
        request: HistoryProjectPageRequest,
    ) -> Result<HistoryProjectPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.history_projects(request)
    }

    /// List transcript-backed canonical sessions for one opaque project
    /// identity. Native index enrichment remains separately identified.
    pub fn history_sessions(
        &self,
        request: HistorySessionPageRequest,
    ) -> Result<HistorySessionPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.history_sessions(request)
    }

    /// Execute one snapshot-bound RFC 012B library page on the persistent
    /// read-only query pool. Public transports must construct this request
    /// through the checked catalog contract boundary.
    pub(crate) fn catalog_page(
        &self,
        request: CatalogPageQueryRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<CatalogRetainedPageOutcome, EngineError> {
        let queries = self.query_client()?;
        queries.catalog_page(request, cancellation)
    }

    /// Resolve one persisted RFC 012B external reference against the current
    /// restart-authenticated Ready snapshot.
    pub(crate) fn resolve_catalog_entity(
        &self,
        request: CatalogResolutionQueryRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<crate::catalog_contract::page::CatalogEntityResolutionResponse, EngineError> {
        let queries = self.query_client()?;
        queries.catalog_resolution(request, cancellation)
    }

    /// Read one transcript-backed canonical session plus counts and decisive
    /// provenance from one committed snapshot.
    pub fn session_details(
        &self,
        request: SessionDetailsRequest,
    ) -> Result<SessionDetails, EngineError> {
        let (_, queries) = self.clients()?;
        queries.session_details(request)
    }

    pub fn session_details_cancellable(
        &self,
        request: SessionDetailsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<SessionDetails, EngineError> {
        let (_, queries) = self.clients()?;
        queries.session_details_cancellable(request, cancellation)
    }

    /// Page canonical messages in deterministic source-time/key order. Both
    /// row count and returned payload bytes are bounded by Rust.
    pub fn messages(&self, request: MessagePageRequest) -> Result<MessagePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.messages(request)
    }

    pub fn messages_cancellable(
        &self,
        request: MessagePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<MessagePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.messages_cancellable(request, cancellation)
    }

    /// Search one writer-maintained canonical FTS projection across root and
    /// delegated messages with one score domain and exact totals.
    pub fn search_cancellable(
        &self,
        request: SearchPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<SearchPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.search_cancellable(request, cancellation)
    }

    /// Page a verified session's canonical root and delegated messages with
    /// exact session facets and decisive parent-message branch anchors.
    pub fn timeline_cancellable(
        &self,
        request: TimelinePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TimelinePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.timeline_cancellable(request, cancellation)
    }

    /// Page current child-run delegation relations for one verified session.
    pub fn delegations_cancellable(
        &self,
        request: DelegationPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<DelegationPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.delegations_cancellable(request, cancellation)
    }

    /// Page canonical workflow containers for one verified session.
    pub fn workflows_cancellable(
        &self,
        request: WorkflowPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<WorkflowPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.workflows_cancellable(request, cancellation)
    }

    /// Read one canonical workflow container and its bounded native snapshot.
    pub fn workflow_details_cancellable(
        &self,
        request: WorkflowDetailsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<WorkflowDetails, EngineError> {
        let (_, queries) = self.clients()?;
        queries.workflow_details_cancellable(request, cancellation)
    }

    /// Page current workflow-member evidence without inferring child terminal
    /// state from orchestration completion or result payloads.
    pub fn workflow_members_cancellable(
        &self,
        request: WorkflowMemberPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<WorkflowMemberPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.workflow_members_cancellable(request, cancellation)
    }

    /// Page canonical project-memory documents, including exact content and
    /// decisive provenance, under Rust-enforced row and byte bounds.
    pub fn memory_documents_cancellable(
        &self,
        request: MemoryDocumentPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<MemoryDocumentPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.memory_documents_cancellable(request, cancellation)
    }

    /// Page canonical task collections globally or under one trustworthy
    /// session, run, or team relation.
    pub fn task_collections_cancellable(
        &self,
        request: TaskCollectionPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TaskCollectionPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.task_collections_cancellable(request, cancellation)
    }

    /// Page canonical task items for one opaque collection identity.
    pub fn tasks_cancellable(
        &self,
        request: TaskPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TaskPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.tasks_cancellable(request, cancellation)
    }

    /// Page global canonical plan documents. Plans remain unscoped until a
    /// native source supplies a trustworthy relation.
    pub fn plans_cancellable(
        &self,
        request: PlanPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<PlanPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.plans_cancellable(request, cancellation)
    }

    /// Page persisted tool-result sidecars for one verified project/session.
    pub fn tool_results_cancellable(
        &self,
        request: ToolResultPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<ToolResultPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.tool_results_cancellable(request, cancellation)
    }

    /// Page session-scoped file-history artifacts and optional base64 content.
    pub fn artifacts_cancellable(
        &self,
        request: ArtifactPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<ArtifactPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.artifacts_cancellable(request, cancellation)
    }

    /// Return all-time canonical usage totals for one project or one verified
    /// session within that project.
    pub fn usage_totals(
        &self,
        request: UsageScopeRequest,
    ) -> Result<UsageTotalsReport, EngineError> {
        let (_, queries) = self.clients()?;
        queries.usage_totals(request)
    }

    pub fn usage_totals_cancellable(
        &self,
        request: UsageScopeRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<UsageTotalsReport, EngineError> {
        let (_, queries) = self.clients()?;
        queries.usage_totals_cancellable(request, cancellation)
    }

    /// Return inclusive daily canonical usage activity plus separately
    /// reported contributions that have no valid source date.
    pub fn usage_activity(
        &self,
        request: UsageActivityRequest,
    ) -> Result<UsageActivityReport, EngineError> {
        let (_, queries) = self.clients()?;
        queries.usage_activity(request)
    }

    pub fn usage_activity_cancellable(
        &self,
        request: UsageActivityRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<UsageActivityReport, EngineError> {
        let (_, queries) = self.clients()?;
        queries.usage_activity_cancellable(request, cancellation)
    }

    /// Page RFC 012C response-level usage shadow state for one verified
    /// session, optionally narrowed to an actor or one present affiliation.
    pub fn runtime_usage_v2_cancellable(
        &self,
        request: RuntimeUsageV2PageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageV2Page, EngineError> {
        let (_, queries) = self.clients()?;
        queries.runtime_usage_v2_cancellable(request, cancellation)
    }

    /// Durable usage-v2 query service plus scoped observer merge.
    ///
    /// Pure overlay join on already-typed contributions. FTS bootstrap does
    /// not gate this path; it never parses native JSON.
    pub(crate) fn merge_runtime_usage_live(
        &self,
        durable: &[runtime_semantic_merge::DurableUsageContribution],
        durable_coverage: &SourceCoverageSet,
        observer_events: &[runtime_semantic_merge::ScopedUsageObserverEvent],
        observer_coverage: &SourceCoverageSet,
    ) -> Result<runtime_semantic_merge::DurableLiveUsageMerge, EngineError> {
        runtime_semantic_merge::merge_durable_and_scoped_usage(
            durable,
            durable_coverage,
            observer_events,
            observer_coverage,
        )
        .map_err(|error| EngineError::InvalidQuery(error.to_string()))
    }

    /// Reconcile any closed RFC 012C family through the same common reducer
    /// used by durable ingestion and scoped projection. This remains a typed,
    /// crate-private reference consumer while public family event contracts
    /// are frozen; it accepts no native payload representation.
    pub(crate) fn merge_runtime_semantic_live(
        &self,
        durable: &[runtime_semantic_merge::DurableRuntimeContribution],
        durable_coverage: &SourceCoverageSet,
        observer_events: &[runtime_semantic_merge::ScopedRuntimeObserverEvent],
        observer_coverage: &SourceCoverageSet,
    ) -> Result<runtime_semantic_merge::DurableLiveRuntimeMerge, EngineError> {
        runtime_semantic_merge::merge_durable_and_scoped_runtime(
            durable,
            durable_coverage,
            observer_events,
            observer_coverage,
        )
        .map_err(|error| EngineError::InvalidQuery(error.to_string()))
    }

    /// Resolve a complete source-selection vector and return exactly one
    /// labeled aggregate arm under the same durable read snapshot.
    pub fn runtime_usage_totals_cancellable(
        &self,
        request: RuntimeUsageTotalsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageTotalsReport, EngineError> {
        let (_, queries) = self.clients()?;
        queries.runtime_usage_totals_cancellable(request, cancellation)
    }

    /// Compare the retained legacy and eligible usage-v2 aggregate arms under
    /// one snapshot. The query records only bounded owner-lifetime counters.
    pub fn runtime_usage_compatibility_cancellable(
        &self,
        request: RuntimeUsageCompatibilityRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageCompatibilityReport, EngineError> {
        let (_, queries) = self.clients()?;
        queries.runtime_usage_compatibility_cancellable(request, cancellation)
    }

    /// Compare-and-set one source instance's runtime usage query selection.
    /// Promotion is guarded by the current Ready projection/complete coverage
    /// barrier; rollback remains available when that projection is unhealthy.
    pub fn select_runtime_usage_query_cancellable(
        &self,
        command: RuntimeUsageQuerySelectionCommand,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeUsageQuerySelectionResult, EngineError> {
        if command.reason.trim().is_empty() || command.reason.len() > 4 * 1024 {
            return Err(EngineError::InvalidConfig(
                "runtime usage query selection requires a bounded reason".to_string(),
            ));
        }
        if command.expected_selected_contract_version == 0
            || command.expected_selected_query_id.trim().is_empty()
            || !matches!(
                command.target_query_id.as_str(),
                runtime_usage_query::LEGACY_USAGE_QUERY_ID
                    | runtime_usage_query::RUNTIME_USAGE_V2_QUERY_ID
            )
        {
            return Err(EngineError::InvalidConfig(
                "runtime usage query selection contains an unsupported query target".to_string(),
            ));
        }
        let target_request = runtime_usage_query::RuntimeUsageQuerySelectionTargetRequest {
            project_id: command.project_id.clone(),
            session_id: command.session_id.clone(),
        };
        let target = self
            .query_client()?
            .runtime_usage_query_selection_target_cancellable(
                target_request.clone(),
                cancellation.clone(),
            )?;
        let requested = QueryPackSelectionValue {
            query_id: command.target_query_id.clone(),
            contract_version: 1,
        };
        if target.selection.selected.query_id == requested.query_id
            && target.selection.selected.contract_version == requested.contract_version
        {
            return Ok(RuntimeUsageQuerySelectionResult {
                contract_version:
                    runtime_usage_query::RUNTIME_USAGE_QUERY_SELECTION_CONTRACT_VERSION,
                at_commit_seq: target.at_commit_seq,
                project_id: command.project_id,
                session_id: command.session_id,
                selection: target.selection,
            });
        }
        if target.selection.materialized != command.expected_materialized
            || target.selection.selected.query_id != command.expected_selected_query_id
            || target.selection.selected.contract_version
                != command.expected_selected_contract_version
            || target.selection.selection_epoch != command.expected_selection_epoch
        {
            return Err(EngineError::InvalidQuery(
                "runtime usage query selection expectation is stale".to_string(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(EngineError::QueryCancelled);
        }
        let current = QueryPackSelectionValue {
            query_id: target.selection.selected.query_id.clone(),
            contract_version: target.selection.selected.contract_version,
        };
        let expected = if target.selection.materialized {
            QueryPackSelectionExpectation::At {
                selected: current.clone(),
                selection_epoch: target.selection.selection_epoch,
            }
        } else {
            QueryPackSelectionExpectation::Absent
        };
        let (rollback, projection_guard) = match command.target_query_id.as_str() {
            runtime_usage_query::RUNTIME_USAGE_V2_QUERY_ID => {
                if current.query_id != runtime_usage_query::LEGACY_USAGE_QUERY_ID
                    || current.contract_version != 1
                {
                    return Err(EngineError::InvalidQuery(
                        "runtime usage-v2 promotion requires the legacy usage selection"
                            .to_string(),
                    ));
                }
                (
                    current,
                    Some(QueryPackProjectionGuard {
                        projection_id: runtime_semantic_projection::USAGE_V2_PROJECTION_ID
                            .to_string(),
                        projection_scope_key: target.stable_key.clone(),
                        projection_version:
                            runtime_semantic_projection::USAGE_V2_PROJECTION_VERSION,
                        coverage_owner_id: runtime_semantic_projection::USAGE_V2_PROJECTION_ID
                            .to_string(),
                        coverage_domain_name: runtime_semantic_projection::USAGE_V2_PROJECTION_ID
                            .to_string(),
                        coverage_domain_version:
                            runtime_semantic_projection::USAGE_V2_PROJECTION_VERSION,
                    }),
                )
            }
            runtime_usage_query::LEGACY_USAGE_QUERY_ID => {
                if !target.selection.materialized
                    || target.selection.rollback.query_id
                        != runtime_usage_query::LEGACY_USAGE_QUERY_ID
                    || target.selection.rollback.contract_version != 1
                {
                    return Err(EngineError::InvalidQuery(
                        "runtime usage query has no retained legacy rollback target".to_string(),
                    ));
                }
                (
                    QueryPackSelectionValue {
                        query_id: runtime_usage_query::LEGACY_USAGE_QUERY_ID.to_string(),
                        contract_version: 1,
                    },
                    None,
                )
            }
            _ => unreachable!("target query id was validated above"),
        };
        let now = engine_now_unix_ms()?;
        self.commit_projection_versions(ProjectionVersionCommit {
            source_instance_id: target.source_instance_id,
            reason: command.reason,
            started_at: now,
            committed_at: now,
            projection_versions: Vec::new(),
            coverage_sets: Vec::new(),
            coverage_preconditions: Vec::new(),
            query_pack_selections: vec![QueryPackSelectionUpdate {
                query_pack_id: runtime_usage_query::RUNTIME_USAGE_QUERY_PACK_ID.to_string(),
                scope_key: target.stable_key,
                expected,
                selected: requested,
                rollback,
                projection_guard,
            }],
        })?;
        let selected = self
            .query_client()?
            .runtime_usage_query_selection_target_cancellable(
                target_request,
                QueryCancellationToken::default(),
            )?;
        Ok(RuntimeUsageQuerySelectionResult {
            contract_version: runtime_usage_query::RUNTIME_USAGE_QUERY_SELECTION_CONTRACT_VERSION,
            at_commit_seq: selected.at_commit_seq,
            project_id: command.project_id,
            session_id: command.session_id,
            selection: selected.selection,
        })
    }

    pub fn fact_family_coverage_cancellable(
        &self,
        request: FactFamilyCoveragePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<FactFamilyCoveragePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.fact_family_coverage_cancellable(request, cancellation)
    }

    /// Return one keyset-paged snapshot of durable run state and current
    /// native presence evidence. No process liveness is assessed here.
    pub fn runtime_snapshot(
        &self,
        request: RuntimeSnapshotRequest,
    ) -> Result<RuntimeSnapshot, EngineError> {
        let (_, queries) = self.clients()?;
        queries.runtime_snapshot(request)
    }

    pub fn runtime_snapshot_cancellable(
        &self,
        request: RuntimeSnapshotRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RuntimeSnapshot, EngineError> {
        let (_, queries) = self.clients()?;
        queries.runtime_snapshot_cancellable(request, cancellation)
    }

    /// Look up one canonical run using the same evidence model as runtime
    /// snapshots. Unknown but well-formed identifiers return no row.
    pub fn run_state(&self, request: RunStateRequest) -> Result<RunStateLookup, EngineError> {
        let (_, queries) = self.clients()?;
        queries.run_state(request)
    }

    pub fn run_state_cancellable(
        &self,
        request: RunStateRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<RunStateLookup, EngineError> {
        let (_, queries) = self.clients()?;
        queries.run_state_cancellable(request, cancellation)
    }

    /// List configured source instances with catalog/error/commit counts.
    pub fn sources(&self, request: SourcePageRequest) -> Result<SourcePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.sources(request)
    }

    pub fn sources_cancellable(
        &self,
        request: SourcePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<SourcePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.sources_cancellable(request, cancellation)
    }

    /// Return one coherent set of canonical and source-catalog counts.
    pub fn canonical_stats(&self) -> Result<CanonicalStats, EngineError> {
        self.canonical_stats_cancellable(QueryCancellationToken::default())
    }

    pub fn canonical_stats_cancellable(
        &self,
        cancellation: QueryCancellationToken,
    ) -> Result<CanonicalStats, EngineError> {
        let (writer, queries) = self.clients()?;
        // Performance counters are intentionally owner scoped rather
        // than part of the SQLite read transaction. Sample them first so this
        // `getStats` request does not measure itself.
        let performance = EnginePerformanceSnapshot {
            writer: writer.performance_snapshot(),
            queries: queries.performance_snapshot(),
            source: self.source_telemetry.snapshot(),
            storage: StoragePerformanceSnapshot {
                database_file_bytes: file_size(&self.database_path),
                wal_file_bytes: file_size(&sqlite_sidecar_path(&self.database_path, "-wal")),
                shared_memory_file_bytes: file_size(&sqlite_sidecar_path(
                    &self.database_path,
                    "-shm",
                )),
            },
        };
        let mut stats = queries.canonical_stats_cancellable(cancellation)?;
        stats.performance = Some(performance);
        Ok(stats)
    }

    pub fn teams(&self, request: TeamPageRequest) -> Result<TeamPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.teams(request)
    }

    pub fn teams_cancellable(
        &self,
        request: TeamPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.teams_cancellable(request, cancellation)
    }

    pub fn team_details(&self, request: TeamDetailsRequest) -> Result<TeamDetails, EngineError> {
        let (_, queries) = self.clients()?;
        queries.team_details(request)
    }

    pub fn team_details_cancellable(
        &self,
        request: TeamDetailsRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamDetails, EngineError> {
        let (_, queries) = self.clients()?;
        queries.team_details_cancellable(request, cancellation)
    }

    pub fn team_inboxes(
        &self,
        request: TeamInboxPageRequest,
    ) -> Result<TeamInboxPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.team_inboxes(request)
    }

    pub fn team_inboxes_cancellable(
        &self,
        request: TeamInboxPageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamInboxPage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.team_inboxes_cancellable(request, cancellation)
    }

    pub fn team_inbox_messages(
        &self,
        request: TeamInboxMessagePageRequest,
    ) -> Result<TeamInboxMessagePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.team_inbox_messages(request)
    }

    pub fn team_inbox_messages_cancellable(
        &self,
        request: TeamInboxMessagePageRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<TeamInboxMessagePage, EngineError> {
        let (_, queries) = self.clients()?;
        queries.team_inbox_messages_cancellable(request, cancellation)
    }

    /// Atomically persist one decoded source range, advance its durable
    /// cursor, update projection readiness, and append public changes.
    pub(crate) fn commit_observation(
        &self,
        request: ObservationCommit,
    ) -> Result<CommitReceipt, EngineError> {
        let receipt = self
            .submit_observation(request)?
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })??;
        self.accept_commit_receipt(&receipt);
        Ok(receipt)
    }

    pub(crate) fn submit_observation(
        &self,
        request: ObservationCommit,
    ) -> Result<crossbeam_channel::Receiver<Result<CommitReceipt, EngineError>>, EngineError> {
        self.writer_client()?.submit_observation(request)
    }

    /// Allocate the durable source-instance identity before adapters derive
    /// entity keys for a newly discovered instance.
    pub(crate) fn reserve_source_instance(
        &self,
        source: commit::SourceInstanceSpec,
    ) -> Result<u64, EngineError> {
        let writer = self.writer_client()?;
        writer.reserve_source_instance(source)
    }

    /// Advance common projection readiness on the same durable commit clock as
    /// ingestion. Equal transitions are suppressed by the writer.
    pub(crate) fn commit_projection_versions(
        &self,
        request: ProjectionVersionCommit,
    ) -> Result<Option<u64>, EngineError> {
        let writer = self.writer_client()?;
        let receipt = writer.commit_projection_versions(request)?;
        if let Some(receipt) = receipt {
            self.commit_notifications.publish(receipt.commit_seq);
            Ok(Some(receipt.commit_seq))
        } else {
            Ok(None)
        }
    }

    /// Register, schedule, or begin an ordinary refresh of the source-neutral
    /// RFC 012B Library build on the same durable clock as observation
    /// commits. Refresh start retains the exact current Ready snapshot; this
    /// internal seam cannot publish new coverage or catalog rows.
    pub(crate) fn commit_catalog_build_state(
        &self,
        command: CatalogBuildStateCommand,
    ) -> Result<Option<u64>, EngineError> {
        let writer = self.writer_client()?;
        let receipt = writer.commit_catalog_build_state(command)?;
        if let Some(receipt) = receipt {
            self.commit_notifications.publish(receipt.commit_seq);
            Ok(Some(receipt.commit_seq))
        } else {
            Ok(None)
        }
    }

    /// Atomically publish the checked initial RFC 012B Library snapshot and
    /// transition its exact durable Building lineage to Ready. The command is
    /// crate-private and carries no public query or N-API authority.
    pub(crate) fn commit_initial_catalog_publication(
        &self,
        command: CatalogInitialPublicationCommand,
    ) -> Result<Option<CatalogInitialPublicationReceipt>, EngineError> {
        let writer = self.writer_client()?;
        let receipt = writer.commit_initial_catalog_publication(command)?;
        if let Some(receipt) = &receipt {
            self.commit_notifications.publish(receipt.commit_seq);
        }
        Ok(receipt)
    }

    /// Atomically publish one checked ordinary-refresh successor while
    /// retaining the predecessor snapshot and its already-issued read
    /// authority. This remains crate-private and grants no public query or
    /// N-API access.
    pub(crate) fn commit_refresh_catalog_publication(
        &self,
        command: CatalogRefreshPublicationCommand,
    ) -> Result<Option<CatalogRefreshPublicationReceipt>, EngineError> {
        let writer = self.writer_client()?;
        let receipt = writer.commit_refresh_catalog_publication(command)?;
        if let Some(receipt) = &receipt {
            self.commit_notifications.publish(receipt.commit_seq);
        }
        Ok(receipt)
    }

    /// Atomically record one append-only logical query-retirement decision.
    /// Snapshot headers and frames remain restart evidence; this grants no
    /// physical-deletion or public policy authority.
    pub(crate) fn retire_catalog_snapshot(
        &self,
        command: CatalogSnapshotRetirementCommand,
    ) -> Result<Option<CatalogSnapshotRetirementReceipt>, EngineError> {
        let writer = self.writer_client()?;
        let receipt = writer.retire_catalog_snapshot(command)?;
        if let Some(receipt) = &receipt {
            self.commit_notifications.publish(receipt.commit_seq);
        }
        Ok(receipt)
    }

    /// Commit storage-agnostic adapter facts through the common projectors.
    /// Change-log entries and durable fact counts are derived by the engine.
    pub(crate) fn commit_facts(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<CommitReceipt, EngineError> {
        let receipt = self
            .submit_facts(request, batch)?
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })??;
        self.accept_commit_receipt(&receipt);
        Ok(receipt)
    }

    pub(crate) fn submit_facts(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<crossbeam_channel::Receiver<Result<CommitReceipt, EngineError>>, EngineError> {
        self.writer_client()?.submit_facts(request, batch)
    }

    pub(crate) fn accept_commit_receipt(&self, receipt: &CommitReceipt) {
        self.commit_notifications.publish(receipt.commit_seq);
        if let Ok(writer) = self.writer_client() {
            writer.record_changes_published(receipt.change_count);
        }
    }

    /// Replay durable projection changes from a snapshot-consistent read-only
    /// query lane. The returned watermark and page come from one transaction.
    pub fn replay_changes(
        &self,
        request: ChangeReplayRequest,
    ) -> Result<ChangeReplay, EngineError> {
        let (_, queries) = self.clients()?;
        queries.replay_changes(request)
    }

    pub fn replay_changes_cancellable(
        &self,
        request: ChangeReplayRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<ChangeReplay, EngineError> {
        let (_, queries) = self.clients()?;
        queries.replay_changes_cancellable(request, cancellation)
    }

    /// Wait for the sole Rust writer to publish a commit newer than the
    /// supplied watermark. This never opens or queries SQLite while idle.
    pub async fn wait_for_commit(
        &self,
        after_commit_seq: u64,
        timeout_ms: u32,
        cancellation: QueryCancellationToken,
    ) -> Result<CommitWaitResult, EngineError> {
        self.commit_notifications
            .wait(after_commit_seq, timeout_ms, &cancellation)
            .await
    }

    /// Run durable outbox maintenance through the sole writer lane. Normal
    /// commits apply the default policy automatically; this entry point lets
    /// a host run the same policy during an explicit maintenance tick.
    pub fn maintain_change_log(
        &self,
        policy: ChangeLogRetentionPolicy,
        now_ms: i64,
    ) -> Result<ChangeLogRetentionSnapshot, EngineError> {
        let writer = self.writer_client()?;
        writer.maintain_change_log(policy, now_ms)
    }

    /// Hydrate one adapter instance's durable common-source state through the
    /// bounded read-only lane. The observation coordinator uses this to resume
    /// common drivers; it is intentionally not part of the public query API.
    pub(crate) fn source_catalog(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
    ) -> Result<SourceCatalogSnapshot, EngineError> {
        self.writer_client()?.source_catalog(adapter_id, stable_key)
    }

    pub(crate) fn source_coverage_replay_baseline(
        &self,
        source_instance_id: u64,
        owner_id: &str,
        owner_scope_key: &[u8],
        family: &str,
        version: u32,
    ) -> Result<Option<SourceCoverageReplayBaseline>, EngineError> {
        self.query_client()?.source_coverage_replay_baseline(
            source_instance_id,
            owner_id,
            owner_scope_key,
            family,
            version,
        )
    }

    pub(crate) fn source_performance_recorder(
        &self,
        adapter_id: &str,
        stream_id: &str,
        driver_kind: &str,
    ) -> SourcePerformanceRecorder {
        self.source_telemetry
            .recorder(adapter_id, stream_id, driver_kind)
    }

    pub(crate) fn latest_commit_seq(&self) -> u64 {
        self.commit_notifications.latest_commit_seq()
    }

    pub fn cancel_pending_queries(&self) -> Result<u64, EngineError> {
        let (_, queries) = self.clients()?;
        Ok(queries.cancel_pending())
    }

    pub fn reconcile_adapter(
        self: &Arc<Self>,
        adapter_id: &str,
        request: ReconcileRequest,
    ) -> Result<ReconcileOutcome, EngineError> {
        self.reconcile_adapter_cancellable(adapter_id, request, QueryCancellationToken::default())
    }

    pub fn reconcile_adapter_cancellable(
        self: &Arc<Self>,
        adapter_id: &str,
        request: ReconcileRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<ReconcileOutcome, EngineError> {
        let adapter = self.registered_adapter(adapter_id)?;
        ObservationCoordinator::with_cancellation(Arc::clone(self), cancellation)
            .reconcile(adapter.as_ref(), request)
    }

    /// Execute one explicitly authorized, source-instance-scoped fact-family
    /// replacement through the same common discovery, drivers, decoder, and
    /// writer lane as ordinary observation.
    pub fn replay_fact_family_cancellable(
        self: &Arc<Self>,
        command: FactFamilyReplayCommand,
        cancellation: QueryCancellationToken,
    ) -> Result<FactFamilyReplayResult, EngineError> {
        if command.configured_roots.is_empty()
            || command.reason.trim().is_empty()
            || command.reason.len() > 4 * 1024
        {
            return Err(EngineError::InvalidConfig(
                "fact-family replay requires configured roots and a bounded reason".to_string(),
            ));
        }
        if command.owner_id != runtime_semantic_projection::USAGE_V2_PROJECTION_ID
            || command.family != runtime_semantic_projection::USAGE_V2_PROJECTION_ID
            || command.family_version != runtime_semantic_projection::USAGE_V2_PROJECTION_VERSION
        {
            return Err(EngineError::InvalidConfig(format!(
                "fact-family replay is not implemented for owner {} family {} version {}",
                command.owner_id, command.family, command.family_version
            )));
        }
        let target = self.query_client()?.fact_family_replay_target_cancellable(
            coverage_query::FactFamilyReplayTargetRequest {
                project_id: command.project_id.clone(),
                session_id: command.session_id.clone(),
                owner_id: command.owner_id.clone(),
                family: command.family.clone(),
                family_version: command.family_version,
                adapter_id: command.adapter_id.clone(),
                expected_source_instance_ref: command.expected_source_instance_ref.clone(),
                expected_content_digest_ref: command.expected_content_digest_ref.clone(),
                expected_coverage_last_commit_seq: command.expected_coverage_last_commit_seq,
            },
            cancellation.clone(),
        )?;
        if target.source_instance_id == 0 || target.adapter_id != command.adapter_id {
            return Err(EngineError::InvalidQuery(
                "fact-family replay target is invalid or belongs to another adapter".to_string(),
            ));
        }
        let adapter = self.registered_adapter(&command.adapter_id)?;
        let request = FactFamilyReplayRequest::usage_v2(command.reason.clone()).authorized(
            target.adapter_id.clone(),
            target.canonical_source_instance_key,
            target.content_digest,
            target.coverage_last_commit_seq,
        );
        let outcome = ObservationCoordinator::with_cancellation(Arc::clone(self), cancellation)
            .replay_discovered_fact_family(
                adapter.as_ref(),
                command.configured_roots,
                &target.stable_key,
                request,
            )?;
        Ok(FactFamilyReplayResult {
            contract_version: FACT_FAMILY_REPLAY_COMMAND_CONTRACT_VERSION,
            project_id: command.project_id,
            session_id: command.session_id,
            owner_id: command.owner_id,
            family: command.family,
            family_version: command.family_version,
            authorized_source_instance_ref: target.source_instance_ref,
            authorized_content_digest_ref: target.content_digest_ref,
            authorized_coverage_last_commit_seq: target.coverage_last_commit_seq,
            outcome,
        })
    }

    pub fn start_registered_observation(
        self: &Arc<Self>,
        adapter_id: &str,
        options: ObservationSupervisorOptions,
    ) -> Result<(), EngineError> {
        self.start_registered_observation_cancellable(
            adapter_id,
            options,
            QueryCancellationToken::default(),
        )
    }

    pub fn start_registered_observation_cancellable(
        self: &Arc<Self>,
        adapter_id: &str,
        options: ObservationSupervisorOptions,
        cancellation: QueryCancellationToken,
    ) -> Result<(), EngineError> {
        let adapter = self.registered_adapter(adapter_id)?;
        self.start_observation_supervisor_cancellable(adapter, options, cancellation)
    }

    fn registered_adapter(
        &self,
        adapter_id: &str,
    ) -> Result<Arc<dyn crate::adapter::AgentAdapter>, EngineError> {
        self.adapters
            .resolve(adapter_id)
            .map_err(|error| EngineError::InvalidConfig(error.to_string()))
    }

    /// Run the bounded native probe before any verified durable source read.
    /// Unsupported and candidate artifacts intentionally receive no typed
    /// authority; callers may retain legacy ingestion, but must not publish
    /// promoted fact-family coverage from that path.
    pub(crate) fn durable_authorization_for_roots(
        &self,
        adapter_id: &str,
        roots: &[PathBuf],
    ) -> Result<Option<TypedAccessAuthorization>, EngineError> {
        if !self.adapters.has_verified_support_catalog() {
            return Ok(None);
        }
        let adapter_id = AdapterId::new(adapter_id)
            .map_err(|error| EngineError::InvalidConfig(error.to_string()))?;
        let Some(probe) = self
            .adapters
            .probe_native_support(&adapter_id, roots)
            .map_err(|error| EngineError::InvalidConfig(error.to_string()))?
        else {
            return Ok(None);
        };
        self.adapters
            .authorize_durable_if_supported(&adapter_id, &probe)
            .map_err(|error| EngineError::InvalidConfig(error.to_string()))
    }

    pub(crate) fn uses_verified_support_catalog(&self) -> bool {
        self.adapters.has_verified_support_catalog()
    }

    /// Retain a lossless, bounded dirty marker for one discovered instance.
    /// Native watchers and polling supervisors use this before attempting the
    /// same coordinator entry point used by explicit reconcile requests.
    pub(crate) fn mark_observation_instance_dirty(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
        reason: crate::source::DirtyReason,
    ) -> Result<(), EngineError> {
        self.observation
            .mark_instance_dirty(adapter_id, stable_key, reason)
    }

    /// Retain a lossless, bounded dirty marker for one known source object.
    /// Native content events use this path so an append does not rescan every
    /// object beneath the adapter root.
    pub(crate) fn mark_observation_object_dirty(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
        stream_key: &str,
        object_key: &[u8],
        reason: crate::source::DirtyReason,
    ) -> Result<(), EngineError> {
        self.observation
            .mark_object_dirty(adapter_id, stable_key, stream_key, object_key, reason)
    }

    /// Escalate an adapter to a full discovery/reconcile pass. Overflow and
    /// watcher-backend failure must use this instead of dropping invalidation.
    pub(crate) fn require_observation_reconcile(
        &self,
        adapter_id: &str,
        reason: crate::source::DirtyReason,
    ) -> Result<(), EngineError> {
        self.observation.mark_adapter_dirty(adapter_id, reason)
    }

    pub(crate) fn next_observation_work(&self, adapter_id: &str) -> Option<PendingObservationWork> {
        self.observation.next_pending(adapter_id)
    }

    pub fn start_observation_supervisor<A: crate::adapter::AgentAdapter>(
        self: &Arc<Self>,
        adapter: A,
        options: ObservationSupervisorOptions,
    ) -> Result<(), EngineError> {
        self.start_observation_supervisor_cancellable(
            adapter,
            options,
            QueryCancellationToken::default(),
        )
    }

    pub fn start_observation_supervisor_cancellable<A: crate::adapter::AgentAdapter>(
        self: &Arc<Self>,
        adapter: A,
        options: ObservationSupervisorOptions,
        cancellation: QueryCancellationToken,
    ) -> Result<(), EngineError> {
        if cancellation.is_cancelled() {
            return Err(EngineError::QueryCancelled);
        }
        let adapter_id = adapter.manifest().id.as_str().to_string();
        let lifecycle = self.lock_lifecycle();
        if lifecycle.phase != LifecyclePhase::Running {
            return Err(EngineError::ShuttingDown);
        }
        let supervisors = self.lock_supervisors();
        if supervisors
            .iter()
            .any(|supervisor| supervisor.adapter_id() == adapter_id)
        {
            return Err(EngineError::InvalidConfig(format!(
                "observation supervisor for adapter {adapter_id} is already running"
            )));
        }
        drop(supervisors);
        drop(lifecycle);
        let supervisor = ObservationSupervisor::start_cancellable(
            Arc::clone(self),
            adapter,
            options,
            cancellation.clone(),
        )?;
        if cancellation.is_cancelled() {
            drop(supervisor);
            return Err(EngineError::QueryCancelled);
        }
        let lifecycle = self.lock_lifecycle();
        if lifecycle.phase != LifecyclePhase::Running {
            drop(lifecycle);
            drop(supervisor);
            return Err(EngineError::ShuttingDown);
        }
        let mut supervisors = self.lock_supervisors();
        if supervisors
            .iter()
            .any(|existing| existing.adapter_id() == adapter_id)
        {
            drop(supervisors);
            drop(lifecycle);
            drop(supervisor);
            return Err(EngineError::InvalidConfig(format!(
                "observation supervisor for adapter {adapter_id} is already running"
            )));
        }
        supervisors.push(supervisor);
        Ok(())
    }

    pub fn refresh_observation_supervisor(&self, adapter_id: &str) -> Result<(), EngineError> {
        self.refresh_observation_supervisor_cancellable(
            adapter_id,
            QueryCancellationToken::default(),
        )
    }

    pub fn refresh_observation_supervisor_cancellable(
        &self,
        adapter_id: &str,
        cancellation: QueryCancellationToken,
    ) -> Result<(), EngineError> {
        if cancellation.is_cancelled() {
            return Err(EngineError::QueryCancelled);
        }
        let supervisors = self.lock_supervisors();
        let client = supervisors
            .iter()
            .find(|supervisor| supervisor.adapter_id() == adapter_id)
            .ok_or_else(|| {
                EngineError::InvalidConfig(format!(
                    "observation supervisor for adapter {adapter_id} is not running"
                ))
            })?
            .client();
        drop(supervisors);
        client.refresh_cancellable(cancellation)
    }

    pub fn stop_observation_supervisor(&self, adapter_id: &str) -> Result<bool, EngineError> {
        let mut supervisors = self.lock_supervisors();
        let Some(index) = supervisors
            .iter()
            .position(|supervisor| supervisor.adapter_id() == adapter_id)
        else {
            return Ok(false);
        };
        let mut supervisor = supervisors.swap_remove(index);
        drop(supervisors);
        supervisor.shutdown()?;
        Ok(true)
    }

    /// Rebuild deferred FTS structures and clear the complete-only search
    /// gate. Catalog/history reads are already admitted. Idempotent.
    pub fn complete_query_bootstrap(&self) -> Result<Option<u64>, EngineError> {
        let (writer, supervisor_clients) = {
            let lifecycle = self.lock_lifecycle();
            if lifecycle.phase != LifecyclePhase::Running {
                return Err(EngineError::ShuttingDown);
            }
            let runtime = lifecycle
                .runtime
                .as_ref()
                .expect("running engine must own its runtime");
            if !runtime.bootstrap_active {
                return Ok(None);
            }
            let clients = self
                .lock_supervisors()
                .iter()
                .map(ObservationSupervisor::client)
                .collect::<Vec<_>>();
            (runtime.writer.client(), clients)
        };

        let mut paused = Vec::with_capacity(supervisor_clients.len());
        for client in supervisor_clients {
            paused.push(client.pause_for_bootstrap()?);
        }
        let finalization = writer.finalize_query_bootstrap();
        let mut resume_result = Ok(());
        for supervisor in paused {
            if let Err(error) = supervisor.resume() {
                if resume_result.is_ok() {
                    resume_result = Err(error);
                }
            }
        }
        let snapshot_watermark = finalization?;
        resume_result?;

        let mut lifecycle = self.lock_lifecycle();
        if lifecycle.phase != LifecyclePhase::Running {
            return Err(EngineError::ShuttingDown);
        }
        let runtime = lifecycle
            .runtime
            .as_mut()
            .expect("running engine must own its runtime");
        if !runtime.bootstrap_active {
            return Ok(snapshot_watermark);
        }
        if runtime.queries.is_none() {
            runtime.queries = Some(QueryPool::start(
                self.database_path.clone(),
                self.query_workers,
                self.source_pass_pool.clone(),
            )?);
        }
        runtime.bootstrap_active = false;
        let latest_commit_seq = runtime
            .queries
            .as_ref()
            .expect("query pool admitted for catalog-first reads")
            .client()
            .overview()?
            .commit_seq;
        drop(lifecycle);
        self.commit_notifications.publish(latest_commit_seq);
        Ok(snapshot_watermark)
    }

    /// Stop accepting work, cancel queued queries, join readers, join the
    /// writer, then release ownership. Concurrent callers wait for the first
    /// disposer and observe the same stopped state.
    pub fn shutdown(&self) -> Result<(), EngineError> {
        let mut lifecycle = self.lock_lifecycle();
        loop {
            match lifecycle.phase {
                LifecyclePhase::Running => {
                    lifecycle.phase = LifecyclePhase::Stopping;
                    break;
                }
                LifecyclePhase::Stopping => {
                    lifecycle = self
                        .stopped
                        .wait(lifecycle)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                LifecyclePhase::Stopped => return Ok(()),
            }
        }

        let mut runtime = lifecycle
            .runtime
            .take()
            .expect("running engine must own its runtime");
        drop(lifecycle);
        self.commit_notifications.stop();

        let mut first_error = None;
        let mut supervisors = {
            let mut owned = self.lock_supervisors();
            std::mem::take(&mut *owned)
        };
        for supervisor in &mut supervisors {
            if let Err(error) = supervisor.shutdown() {
                first_error.get_or_insert(error);
            }
        }
        self.observation.stop_and_wait();
        let observation_workers = {
            let mut workers = self
                .observation_workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            workers.take()
        };
        drop(observation_workers);

        if let Some(queries) = runtime.queries.as_mut() {
            if let Err(error) = queries.shutdown() {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = runtime.writer.shutdown() {
            first_error.get_or_insert(error);
        }
        if let Err(error) = runtime.owner_lock.release() {
            first_error.get_or_insert(error);
        }

        let mut lifecycle = self.lock_lifecycle();
        lifecycle.phase = LifecyclePhase::Stopped;
        self.stopped.notify_all();
        drop(lifecycle);

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn clients(&self) -> Result<(WriterClient, QueryClient), EngineError> {
        Ok((self.writer_client()?, self.query_client()?))
    }

    fn writer_client(&self) -> Result<WriterClient, EngineError> {
        let lifecycle = self.lock_lifecycle();
        if lifecycle.phase != LifecyclePhase::Running {
            return Err(EngineError::ShuttingDown);
        }
        let runtime = lifecycle
            .runtime
            .as_ref()
            .expect("running engine must own its runtime");
        Ok(runtime.writer.client())
    }

    fn query_client(&self) -> Result<QueryClient, EngineError> {
        let lifecycle = self.lock_lifecycle();
        if lifecycle.phase != LifecyclePhase::Running {
            return Err(EngineError::ShuttingDown);
        }
        let runtime = lifecycle
            .runtime
            .as_ref()
            .expect("running engine must own its runtime");
        runtime
            .queries
            .as_ref()
            .map(QueryPool::client)
            .ok_or(EngineError::BootstrapInProgress)
    }

    pub(crate) fn begin_full_reconcile(
        &self,
        adapter_id: &str,
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        self.observation.begin_full(adapter_id, started_at_unix_ms)
    }

    pub(crate) fn observation_workers(&self) -> Result<Arc<rayon::ThreadPool>, EngineError> {
        self.observation_workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .cloned()
            .ok_or(EngineError::ShuttingDown)
    }

    pub(crate) fn begin_instance_reconcile(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        self.observation
            .begin_instance(adapter_id, stable_key, started_at_unix_ms)
    }

    pub(crate) fn begin_object_reconcile(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
        stream_key: &str,
        object_key: &[u8],
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        self.observation.begin_object(
            adapter_id,
            stable_key,
            stream_key,
            object_key,
            started_at_unix_ms,
        )
    }

    fn lock_lifecycle(&self) -> MutexGuard<'_, Lifecycle> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_supervisors(&self) -> MutexGuard<'_, Vec<ObservationSupervisor>> {
        self.supervisors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn engine_now_unix_ms() -> Result<i64, EngineError> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        EngineError::InvalidConfig("system clock precedes the Unix epoch".to_string())
    })?;
    i64::try_from(elapsed.as_millis()).map_err(|_| {
        EngineError::InvalidConfig("system clock exceeds the supported range".to_string())
    })
}

impl Drop for SpaghettiEngineCore {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn normalize_database_path(path: &Path) -> Result<PathBuf, EngineError> {
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return Err(EngineError::InvalidConfig(
            "persistent engine requires a file-backed database path".to_string(),
        ));
    }
    if path.is_dir() {
        return Err(EngineError::InvalidConfig(format!(
            "database path is a directory: {}",
            path.display()
        )));
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| EngineError::InvalidConfig(error.to_string()))?
            .join(path)
    };
    if absolute.exists() {
        return absolute.canonicalize().map_err(|error| {
            EngineError::InvalidConfig(format!(
                "could not resolve database path {}: {error}",
                absolute.display()
            ))
        });
    }
    let file_name = absolute.file_name().ok_or_else(|| {
        EngineError::InvalidConfig(format!(
            "database path has no file name: {}",
            path.display()
        ))
    })?;
    let parent = absolute.parent().ok_or_else(|| {
        EngineError::InvalidConfig(format!("database path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        EngineError::InvalidConfig(format!(
            "could not create database directory {}: {error}",
            parent.display()
        ))
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        EngineError::InvalidConfig(format!(
            "could not resolve database directory {}: {error}",
            parent.display()
        ))
    })?;
    Ok(canonical_parent.join(file_name))
}

fn sqlite_sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut value = database_path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn duration_ms(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SharedSourcePassPool;
    use std::sync::mpsc;
    use std::thread;
    use tempfile::tempdir;

    fn options(database_path: PathBuf) -> EngineOptions {
        EngineOptions {
            database_path,
            query_workers: Some(2),
            owner_label: Some("engine-test".to_string()),
            defer_query_structures: false,
            source_pass_pool: None,
        }
    }

    #[test]
    fn opens_reports_typed_health_and_disposes_without_handles() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("engine.db");
        let engine = SpaghettiEngineCore::open(options(database.clone())).unwrap();

        let status = engine.status();
        assert_eq!(status.state, "running");
        assert!(status.accepting_queries && status.writer_alive);
        assert_eq!(status.alive_query_workers, 2);
        assert_eq!(status.observation.state, "idle");
        assert_eq!(status.owner.unwrap().owner_label, "engine-test");

        let health = engine.health();
        assert!(health.healthy, "{:?}", health.detail);
        let overview = engine.overview().unwrap();
        assert_eq!(overview.schema_version, crate::core::schema::SCHEMA_VERSION);
        assert!(overview.query_only && overview.read_only);

        engine.shutdown().unwrap();
        let stopped = engine.status();
        assert_eq!(stopped.state, "stopped");
        assert_eq!(stopped.observation.state, "stopped");
        assert!(!stopped.writer_alive);
        assert_eq!(stopped.alive_query_workers, 0);
        assert!(matches!(engine.overview(), Err(EngineError::ShuttingDown)));

        let reopened = SpaghettiEngineCore::open(options(database)).unwrap();
        reopened.shutdown().unwrap();
    }

    #[test]
    fn bootstrap_admits_catalog_queries_and_withholds_search_until_finalization() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("bootstrap.db");
        let mut bootstrap_options = options(database);
        bootstrap_options.defer_query_structures = true;
        let engine = SpaghettiEngineCore::open(bootstrap_options).unwrap();

        let status = engine.status();
        assert_eq!(status.state, "bootstrapping");
        assert!(status.accepting_queries);
        assert!(status.writer_alive);
        assert_eq!(status.alive_query_workers, 2);
        assert_eq!(engine.overview().unwrap().commit_seq, 0);
        let search = engine.search_cancellable(
            super::search_query::SearchPageRequest {
                text: "anything".to_string(),
                project_id: None,
                session_id: None,
                adapter_ids: Vec::new(),
                roles: Vec::new(),
                native_kinds: Vec::new(),
                branch_kind: None,
                cursor: None,
                limit: 10,
            },
            QueryCancellationToken::default(),
        );
        assert!(matches!(search, Err(EngineError::BootstrapInProgress)));

        assert_eq!(engine.complete_query_bootstrap().unwrap(), Some(0));
        let ready = engine.status();
        assert_eq!(ready.state, "running");
        assert!(ready.accepting_queries);
        assert_eq!(ready.alive_query_workers, 2);
        assert_eq!(engine.overview().unwrap().commit_seq, 0);
        assert_eq!(
            engine.overview().unwrap().journal_mode.to_ascii_lowercase(),
            "wal",
            "bootstrap finalization must remain on WAL instead of rewriting the file in DELETE mode"
        );
        assert_eq!(engine.complete_query_bootstrap().unwrap(), None);
        engine.shutdown().unwrap();
    }

    #[test]
    fn progressive_host_readiness_keeps_search_unavailable_until_catalog_and_bootstrap() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("progressive.db");
        let mut bootstrap_options = options(database);
        bootstrap_options.defer_query_structures = true;
        let engine = SpaghettiEngineCore::open(bootstrap_options).unwrap();

        let readiness = engine.progressive_host_readiness();
        assert!(readiness.bootstrap_active);
        assert!(!readiness.search_available);
        assert!(!readiness.catalog_query_ready);
        let status = engine.status();
        assert!(!status.search_available);
        assert!(!status.catalog_query_ready);
        assert!(status.accepting_queries);
        assert_eq!(engine.overview().unwrap().commit_seq, 0);

        engine.complete_query_bootstrap().unwrap();
        let ready = engine.progressive_host_readiness();
        assert!(!ready.bootstrap_active);
        assert!(!ready.search_available);
        assert!(
            !ready.catalog_query_ready,
            "empty DB has no last-complete catalog snapshot"
        );
        engine.shutdown().unwrap();
    }

    #[test]
    fn rfc012_d3_durable_queries_progress_while_search_bootstrap_is_incomplete() {
        let dir = tempdir().unwrap();
        let mut bootstrap_options = options(dir.path().join("d3-fair.db"));
        bootstrap_options.defer_query_structures = true;
        let engine = SpaghettiEngineCore::open(bootstrap_options).unwrap();
        assert!(engine.status().accepting_queries);
        assert!(!engine.status().search_available);
        let _overview = engine.overview().unwrap();
        let search_request = super::search_query::SearchPageRequest {
            text: "anything".to_string(),
            project_id: None,
            session_id: None,
            adapter_ids: Vec::new(),
            roles: Vec::new(),
            native_kinds: Vec::new(),
            branch_kind: None,
            cursor: None,
            limit: 10,
        };
        assert!(matches!(
            engine.search_cancellable(search_request.clone(), QueryCancellationToken::default()),
            Err(EngineError::BootstrapInProgress)
        ));
        engine.complete_query_bootstrap().unwrap();
        assert!(!matches!(
            engine.search_cancellable(search_request, QueryCancellationToken::default()),
            Err(EngineError::BootstrapInProgress)
        ));
        engine.shutdown().unwrap();
    }

    #[test]
    fn rfc012_d3_engine_catalog_query_workers_wait_for_shared_source_pass_pool() {
        let dir = tempdir().unwrap();
        let pool = SharedSourcePassPool::new(1).expect("one shared pass is valid");
        let mut bootstrap_options = options(dir.path().join("d3-pool.db"));
        bootstrap_options.query_workers = Some(1);
        bootstrap_options.source_pass_pool = Some(pool.clone());
        let engine = SpaghettiEngineCore::open(bootstrap_options).unwrap();
        assert_eq!(pool.available_permits(), 1);

        let held = pool.blocking_acquire();
        assert_eq!(pool.available_permits(), 0);
        let (tx, rx) = mpsc::channel();
        let query_engine = Arc::clone(&engine);
        let worker = thread::spawn(move || {
            let result = query_engine.overview();
            let _ = tx.send(result);
        });
        thread::sleep(Duration::from_millis(80));
        assert!(
            rx.try_recv().is_err(),
            "catalog/query workers must occupy the shared permit before serving overview"
        );
        drop(held);
        let overview = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("overview resumes after the shared permit is released")
            .expect("overview succeeds after the shared permit is released");
        assert_eq!(overview.commit_seq, 0);
        worker.join().expect("overview worker returns");
        engine.shutdown().unwrap();
    }

    #[test]
    fn rfc012_x1_emit_complete_only_ingest_trace() {
        use std::time::Instant;

        let dir = tempdir().unwrap();
        let mut bootstrap_options = options(dir.path().join("x1-fts.db"));
        bootstrap_options.defer_query_structures = true;
        let started = Instant::now();
        let engine = SpaghettiEngineCore::open(bootstrap_options).unwrap();
        let catalog_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        assert!(engine.status().accepting_queries);
        let _ = engine.overview().unwrap();
        let history_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let search_request = super::search_query::SearchPageRequest {
            text: "anything".to_string(),
            project_id: None,
            session_id: None,
            adapter_ids: Vec::new(),
            roles: Vec::new(),
            native_kinds: Vec::new(),
            branch_kind: None,
            cursor: None,
            limit: 10,
        };
        assert!(matches!(
            engine.search_cancellable(search_request.clone(), QueryCancellationToken::default()),
            Err(EngineError::BootstrapInProgress)
        ));
        let incomplete_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        engine.complete_query_bootstrap().unwrap();
        assert!(!matches!(
            engine.search_cancellable(search_request, QueryCancellationToken::default()),
            Err(EngineError::BootstrapInProgress)
        ));
        let fts_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        assert!(fts_ms >= incomplete_ms);
        let trace = serde_json::json!({
            "source_test": "rfc012_x1_emit_complete_only_ingest_trace",
            "complete_only_gate": "schema_meta.query_bootstrap_state",
            "milestones": [
                {
                    "label": "catalog-ready",
                    "history_complete": false,
                    "catalog_complete": true,
                    "fts_complete": false,
                    "t_ms": catalog_ms
                },
                {
                    "label": "history-ready",
                    "history_complete": true,
                    "catalog_complete": true,
                    "fts_complete": false,
                    "t_ms": history_ms
                },
                {
                    "label": "fts-incomplete",
                    "history_complete": true,
                    "catalog_complete": true,
                    "fts_complete": false,
                    "t_ms": incomplete_ms
                },
                {
                    "label": "fts-complete",
                    "history_complete": true,
                    "catalog_complete": true,
                    "fts_complete": true,
                    "t_ms": fts_ms
                }
            ]
        });
        let default_path = dir.path().join("fts-bootstrap-trace.json");
        let path =
            std::env::var_os("RFC012_X1_TRACE").map_or(default_path, std::path::PathBuf::from);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut body = serde_json::to_vec_pretty(&trace).unwrap();
        if !body.ends_with(b"\n") {
            body.push(b'\n');
        }
        std::fs::write(&path, body).unwrap();
        engine.shutdown().unwrap();
    }

    #[test]
    fn exclusive_owner_rejects_a_second_engine_with_diagnostics() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("owned.db");
        let first = SpaghettiEngineCore::open(options(database.clone())).unwrap();

        let error = match SpaghettiEngineCore::open(options(database.clone())) {
            Ok(_) => panic!("second engine unexpectedly opened"),
            Err(error) => error,
        };
        match error {
            EngineError::OwnerBusy {
                database_path,
                owner,
                ..
            } => {
                assert_eq!(database_path, database.canonicalize().unwrap_or(database));
                assert_eq!(owner.unwrap().owner_label, "engine-test");
            }
            other => panic!("expected OwnerBusy, got {other:?}"),
        }

        first.shutdown().unwrap();
    }

    #[test]
    fn drop_is_a_last_resort_disposer() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("drop.db");
        {
            let engine = SpaghettiEngineCore::open(options(database.clone())).unwrap();
            assert!(engine.health().healthy);
        }

        let reopened = SpaghettiEngineCore::open(options(database)).unwrap();
        reopened.shutdown().unwrap();
    }

    #[tokio::test]
    async fn commit_notifications_wake_without_polling_and_honor_cancellation() {
        let notifications = Arc::new(CommitNotifications::new(7));
        assert_eq!(notifications.latest_commit_seq(), 7);
        let immediate = notifications
            .wait(6, 1_000, &QueryCancellationToken::default())
            .await
            .unwrap();
        assert_eq!(immediate.observed_commit_seq, 7);
        assert_eq!(immediate.reason, "commit");

        let waiting = Arc::clone(&notifications);
        let waiter = tokio::spawn(async move {
            waiting
                .wait(7, 1_000, &QueryCancellationToken::default())
                .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        notifications.publish(8);
        assert_eq!(notifications.latest_commit_seq(), 8);
        let woken = waiter.await.unwrap().unwrap();
        assert_eq!(woken.observed_commit_seq, 8);
        assert_eq!(woken.reason, "commit");

        let cancellation = QueryCancellationToken::default();
        let cancel_for_waiter = cancellation.clone();
        let waiting = Arc::clone(&notifications);
        let waiter = tokio::spawn(async move { waiting.wait(8, 1_000, &cancel_for_waiter).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancellation.cancel();
        assert!(matches!(
            waiter.await.unwrap(),
            Err(EngineError::QueryCancelled)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_one_owner_lock() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let database = dir.path().join("canonical.db");
        std::fs::File::create(&database).unwrap();
        let alias = dir.path().join("alias.db");
        symlink(&database, &alias).unwrap();

        let first = SpaghettiEngineCore::open(options(database)).unwrap();
        assert!(matches!(
            SpaghettiEngineCore::open(options(alias)),
            Err(EngineError::OwnerBusy { .. })
        ));
        first.shutdown().unwrap();
    }
}
