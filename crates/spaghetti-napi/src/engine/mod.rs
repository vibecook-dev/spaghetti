//! Persistent observation/query engine lifecycle (RFC 011).
//!
//! This module is deliberately free of N-API types. The Node binding in
//! `napi_engine` is one host adapter; a future daemon or embedded desktop host
//! can own the same `SpaghettiEngineCore` and receive identical semantics.

mod artifact_projection;
mod commit;
mod coordinator;
mod detail_query;
mod memory_projection;
mod observation;
mod owner_lock;
mod presence_projection;
mod projection;
mod query_identity;
mod query_pool;
mod runtime_query;
mod session_index_projection;
mod settings_projection;
mod supervisor;
mod task_projection;
mod team_projection;
mod team_query;
mod tool_result_projection;
mod usage_query;
mod workflow_projection;
mod writer;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::adapter::FactBatch;
use crate::claude::ClaudeCodeAdapter;
use commit::{CommitReceipt, ObservationCommit};
pub use coordinator::{ObservationCoordinator, ReconcileOutcome, ReconcileRequest};
pub use detail_query::{
    CanonicalStats, MessageDetail, MessagePage, MessagePageRequest, NamedCount, SessionDetail,
    SessionDetails, SessionDetailsRequest, SessionIndexDetail, SourcePage, SourcePageRequest,
    SourceSummary, DEFAULT_DETAIL_PAGE_LIMIT, DETAIL_QUERY_CONTRACT_VERSION,
    MAX_MESSAGE_PAGE_PAYLOAD_BYTES,
};
pub use observation::ObservationStatusSnapshot;
use observation::{ObservationLease, ObservationRuntime, PendingObservationWork};
use owner_lock::DatabaseOwnerLock;
pub use owner_lock::OwnerMetadata;
pub use query_pool::{
    ChangeCursor, ChangeReplay, ChangeReplayRequest, DurableChange, HistoryProjectIndexSummary,
    HistoryProjectPage, HistoryProjectPageRequest, HistoryProjectSummary,
    HistorySessionIndexSummary, HistorySessionPage, HistorySessionPageRequest,
    HistorySessionSummary, QueryCancellationToken, QueryOverview, DEFAULT_HISTORY_PAGE_LIMIT,
    HISTORY_QUERY_CONTRACT_VERSION,
};
use query_pool::{QueryClient, QueryPool, SourceCatalogSnapshot};
pub use runtime_query::{
    RunStateLookup, RunStateRequest, RuntimePresenceSnapshot, RuntimeRunEvidence,
    RuntimeRunSnapshot, RuntimeSnapshot, RuntimeSnapshotEntry, RuntimeSnapshotRequest,
    DEFAULT_RUNTIME_PAGE_LIMIT, RUNTIME_QUERY_CONTRACT_VERSION,
};
use supervisor::ObservationSupervisor;
pub use supervisor::ObservationSupervisorOptions;
pub use team_query::{
    TeamConfigSummary, TeamDetails, TeamDetailsRequest, TeamInboxMessage, TeamInboxMessagePage,
    TeamInboxMessagePageRequest, TeamInboxPage, TeamInboxPageRequest, TeamInboxSummary, TeamMember,
    TeamPage, TeamPageRequest, TeamSummary, DEFAULT_TEAM_PAGE_LIMIT, TEAM_QUERY_CONTRACT_VERSION,
};
pub use usage_query::{
    UntimedUsageSummary, UsageActivityDay, UsageActivityReport, UsageActivityRequest,
    UsageAggregate, UsageCoverageSummary, UsageScopeRequest, UsageTokenValues, UsageTotalsReport,
    MAX_USAGE_ACTIVITY_DAYS, USAGE_QUERY_CONTRACT_VERSION,
};
use writer::{WriterClient, WriterRuntime};

const DEFAULT_QUERY_WORKERS: usize = 2;
const MAX_QUERY_WORKERS: usize = 16;

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

    #[error("engine is shutting down or already stopped")]
    ShuttingDown,

    #[error("query was cancelled")]
    QueryCancelled,

    #[error("query queue is full")]
    QueryQueueFull,

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
    queries: QueryPool,
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
    pub writer_alive: bool,
    pub configured_query_workers: u32,
    pub alive_query_workers: u32,
    pub in_flight_queries: u32,
    pub observation: ObservationStatusSnapshot,
    pub owner: Option<OwnerMetadata>,
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
    pub writer_data_version: u32,
    pub journal_mode: String,
    pub query_only: bool,
    pub read_only: bool,
}

/// One persistent engine owner for one canonical database.
pub struct SpaghettiEngineCore {
    database_path: PathBuf,
    owner: OwnerMetadata,
    query_workers: usize,
    observation: Arc<ObservationRuntime>,
    supervisors: Mutex<Vec<ObservationSupervisor>>,
    lifecycle: Mutex<Lifecycle>,
    stopped: Condvar,
}

impl SpaghettiEngineCore {
    pub const CLAUDE_ADAPTER_ID: &'static str = concat!("claude", "-code");

    pub fn open(options: EngineOptions) -> Result<Arc<Self>, EngineError> {
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
        let queries = QueryPool::start(database_path.clone(), query_workers)?;

        Ok(Arc::new(Self {
            database_path,
            owner,
            query_workers,
            observation: ObservationRuntime::new(),
            supervisors: Mutex::new(Vec::new()),
            lifecycle: Mutex::new(Lifecycle {
                phase: LifecyclePhase::Running,
                runtime: Some(EngineRuntime {
                    owner_lock,
                    writer,
                    queries,
                }),
            }),
            stopped: Condvar::new(),
        }))
    }

    pub fn status(&self) -> EngineStatusSnapshot {
        let lifecycle = self.lock_lifecycle();
        let (writer_alive, alive_query_workers, in_flight_queries) = lifecycle
            .runtime
            .as_ref()
            .map(|runtime| {
                let writer = runtime.writer.client();
                let queries = runtime.queries.client();
                (
                    writer.is_alive(),
                    queries.alive_workers(),
                    queries.in_flight(),
                )
            })
            .unwrap_or((false, 0, 0));
        let running = lifecycle.phase == LifecyclePhase::Running;
        let mut observation = self.observation.snapshot();
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
        }

        EngineStatusSnapshot {
            state: lifecycle.phase.as_str().to_string(),
            database_path: self.database_path.to_string_lossy().into_owned(),
            accepting_queries: running,
            writer_alive,
            configured_query_workers: usize_to_u32(self.query_workers),
            alive_query_workers: usize_to_u32(alive_query_workers),
            in_flight_queries: usize_to_u32(in_flight_queries),
            observation,
            owner: (lifecycle.phase != LifecyclePhase::Stopped).then(|| self.owner.clone()),
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
        let (_, queries) = self.clients()?;
        queries.canonical_stats()
    }

    pub fn canonical_stats_cancellable(
        &self,
        cancellation: QueryCancellationToken,
    ) -> Result<CanonicalStats, EngineError> {
        let (_, queries) = self.clients()?;
        queries.canonical_stats_cancellable(cancellation)
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
        let (writer, _) = self.clients()?;
        writer.commit_observation(request)
    }

    /// Allocate the durable source-instance identity before adapters derive
    /// entity keys for a newly discovered instance.
    pub(crate) fn reserve_source_instance(
        &self,
        source: commit::SourceInstanceSpec,
    ) -> Result<u64, EngineError> {
        let (writer, _) = self.clients()?;
        writer.reserve_source_instance(source)
    }

    /// Commit storage-agnostic adapter facts through the common projectors.
    /// Change-log entries and durable fact counts are derived by the engine.
    pub(crate) fn commit_facts(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<CommitReceipt, EngineError> {
        let (writer, _) = self.clients()?;
        writer.commit_facts(request, batch)
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

    /// Hydrate one adapter instance's durable common-source state through the
    /// bounded read-only lane. The observation coordinator uses this to resume
    /// common drivers; it is intentionally not part of the public query API.
    pub(crate) fn source_catalog(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
    ) -> Result<SourceCatalogSnapshot, EngineError> {
        let (_, queries) = self.clients()?;
        queries.source_catalog(adapter_id, stable_key)
    }

    pub fn cancel_pending_queries(&self) -> Result<u64, EngineError> {
        let (_, queries) = self.clients()?;
        Ok(queries.cancel_pending())
    }

    pub fn reconcile_claude(
        self: &Arc<Self>,
        request: ReconcileRequest,
    ) -> Result<ReconcileOutcome, EngineError> {
        ObservationCoordinator::new(Arc::clone(self)).reconcile(&ClaudeCodeAdapter::new(), request)
    }

    pub fn start_claude_observation(
        self: &Arc<Self>,
        options: ObservationSupervisorOptions,
    ) -> Result<(), EngineError> {
        self.start_observation_supervisor(ClaudeCodeAdapter::new(), options)
    }

    pub fn refresh_claude_observation(&self) -> Result<(), EngineError> {
        self.refresh_observation_supervisor(Self::CLAUDE_ADAPTER_ID)
    }

    pub fn stop_claude_observation(&self) -> Result<bool, EngineError> {
        self.stop_observation_supervisor(Self::CLAUDE_ADAPTER_ID)
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
        let supervisor = ObservationSupervisor::start(Arc::clone(self), adapter, options)?;
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
        client.refresh()
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

        if let Err(error) = runtime.queries.shutdown() {
            first_error.get_or_insert(error);
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
        let lifecycle = self.lock_lifecycle();
        if lifecycle.phase != LifecyclePhase::Running {
            return Err(EngineError::ShuttingDown);
        }
        let runtime = lifecycle
            .runtime
            .as_ref()
            .expect("running engine must own its runtime");
        Ok((runtime.writer.client(), runtime.queries.client()))
    }

    pub(crate) fn begin_full_reconcile(
        &self,
        adapter_id: &str,
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        self.observation.begin_full(adapter_id, started_at_unix_ms)
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

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn options(database_path: PathBuf) -> EngineOptions {
        EngineOptions {
            database_path,
            query_workers: Some(2),
            owner_label: Some("engine-test".to_string()),
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
