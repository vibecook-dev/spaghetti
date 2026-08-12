//! N-API host adapter for the library-first persistent engine.

use std::path::PathBuf;
use std::sync::Arc;

use napi::bindgen_prelude::{AbortSignal, AsyncTask, Env, Error, Result, Status, Task};
use napi_derive::napi;

use crate::engine::{
    EngineHealthSnapshot, EngineOptions, EngineOverview, EngineStatusSnapshot,
    HistoryProjectIndexSummary, HistoryProjectPage, HistoryProjectPageRequest,
    HistoryProjectSummary, HistorySessionIndexSummary, HistorySessionPage,
    HistorySessionPageRequest, HistorySessionSummary, ObservationStatusSnapshot,
    ObservationSupervisorOptions, OwnerMetadata, ReconcileOutcome, ReconcileRequest,
    SpaghettiEngineCore, DEFAULT_HISTORY_PAGE_LIMIT,
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

fn validate_roots(roots: &[String], operation: &str) -> Result<()> {
    if roots.is_empty() || roots.iter().any(|root| root.trim().is_empty()) {
        return Err(Error::new(
            Status::InvalidArg,
            format!("{operation} requires at least one non-empty root"),
        ));
    }
    Ok(())
}
