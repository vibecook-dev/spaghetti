//! N-API host adapter for the library-first persistent engine.

use std::path::PathBuf;
use std::sync::Arc;

use napi::bindgen_prelude::{
    AbortSignal, AsyncBlock, AsyncBlockBuilder, AsyncTask, Env, Error, Result, Status, Task,
};
use napi_derive::napi;

use crate::adapter::{AdapterError, AdapterRegistry, SupportCatalog, SupportContractError};
use crate::napi_catalog::{
    AwaitObservationStartTask, CatalogProjectsTask, CatalogResolveTask, CatalogSessionsTask,
    CatalogStartup, EngineCatalogPageOptions, EngineCatalogSessionPageOptions, ReadinessTask,
};

use crate::claude::ClaudeCodeAdapter;
use crate::codex::CodexAdapter;
use crate::engine::{
    ArtifactPageRequest, ChangeCursor, ChangeReplayRequest, ConfiguredObservationSource,
    DelegationPageRequest, EngineError, EngineOptions, FactFamilyCoveragePageRequest,
    FactFamilyReplayCommand, HistoryProjectPageRequest, HistorySessionPageRequest,
    MemoryDocumentPageRequest, MessagePageRequest, ObservationSupervisorOptions, PlanPageRequest,
    QueryCancellationToken, ReconcileRequest, RunStateRequest, RuntimeSnapshotRequest,
    SearchPageRequest, SessionDetailsRequest, SourcePageRequest, SpaghettiEngineCore,
    TaskCollectionPageRequest, TaskPageRequest, TeamDetailsRequest, TeamInboxMessagePageRequest,
    TeamInboxPageRequest, TeamPageRequest, TimelinePageRequest, ToolResultPageRequest,
    UsageRequest, UsageWindow, WorkflowDetailsRequest, WorkflowMemberPageRequest,
    WorkflowPageRequest, DEFAULT_CAPABILITY_PAGE_LIMIT, DEFAULT_CHANGE_REPLAY_LIMIT,
    DEFAULT_COMMIT_WAIT_TIMEOUT_MS, DEFAULT_DETAIL_PAGE_LIMIT,
    DEFAULT_FACT_FAMILY_COVERAGE_PAGE_LIMIT, DEFAULT_HISTORY_PAGE_LIMIT,
    DEFAULT_ORCHESTRATION_PAGE_LIMIT, DEFAULT_RUNTIME_PAGE_LIMIT, DEFAULT_SEARCH_PAGE_LIMIT,
    DEFAULT_TEAM_PAGE_LIMIT, DEFAULT_TIMELINE_PAGE_LIMIT,
};
use crate::grok::GrokAdapter;

const CLAUDE_ADAPTER_ID: &str = "claude-code";
const CODEX_ADAPTER_ID: &str = "codex";
const GROK_ADAPTER_ID: &str = "grok";
const DEFAULT_SHARED_SOURCE_PASSES: usize = 4;

pub(crate) fn verified_builtin_support_catalog(
) -> std::result::Result<SupportCatalog, SupportContractError> {
    SupportCatalog::new([
        crate::claude::verified_support_release()?,
        crate::codex::verified_support_release()?,
        crate::grok::verified_support_release()?,
    ])
}

pub(crate) fn verified_builtin_registry(
    support_catalog: Arc<SupportCatalog>,
) -> std::result::Result<AdapterRegistry, AdapterError> {
    AdapterRegistry::builder()
        .register(ClaudeCodeAdapter::new())
        .register(CodexAdapter::new())
        .register(GrokAdapter::new())
        .register_native_support_probe(
            CLAUDE_ADAPTER_ID,
            crate::claude::support_probe::probe_claude_native_artifact,
        )
        .register_native_support_probe(
            CODEX_ADAPTER_ID,
            crate::codex::support_probe::probe_codex_native_artifact,
        )
        .register_native_support_probe(
            GROK_ADAPTER_ID,
            crate::grok::support_probe::probe_grok_native_artifact,
        )
        .build_verified(support_catalog)
}

/// Encode one engine result for the JavaScript boundary.
///
/// Query results cross N-API as a JSON string rather than as a marshalled
/// object graph. Two reasons: it is measurably faster for the page-sized
/// payloads these queries return, and it removes the second declaration of
/// every shape — `#[napi(object)]` would need a Rust mirror of each engine
/// struct, whereas serde reads the engine struct itself and `ts-rs` writes
/// TypeScript from the same definition. Encoding happens inside `compute()`,
/// on the worker thread, so the JavaScript thread only receives the string.
pub(crate) fn encode_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| {
        Error::new(
            Status::GenericFailure,
            format!("could not encode the engine response as JSON: {error}"),
        )
    })
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
pub struct EngineWorkflowPageOptions {
    pub project_id: String,
    pub session_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
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
pub struct EngineTaskPageOptions {
    pub collection_id: String,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
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
pub struct EngineArtifactPageOptions {
    pub session_id: String,
    pub cursor: Option<String>,
    /// Page size. Defaults to 50 and is capped by the Rust query engine.
    pub limit: Option<u32>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineUsageOptions {
    /// Opaque project identity returned by `listHistoryProjects`.
    pub project_id: String,
    /// Optional opaque session identity returned by `listHistorySessions`.
    pub session_id: Option<String>,
    /// Inclusive calendar date in YYYY-MM-DD form. Supplying both `from` and
    /// `to` adds the per-day series and the untimed remainder to the report.
    pub from: Option<String>,
    /// Inclusive calendar date in YYYY-MM-DD form.
    pub to: Option<String>,
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
pub struct EngineFactFamilyReplayOptions {
    /// Open adapter identifier registered by the native composition root.
    pub adapter_id: String,
    /// Configured native data roots understood by the selected adapter.
    pub roots: Vec<String>,
    pub project_id: String,
    pub session_id: String,
    pub owner_id: String,
    pub family: String,
    pub family_version: u32,
    /// Echoed from `getFactFamilyCoverage().coverage.sourceInstanceRef`.
    pub expected_source_instance_ref: String,
    /// Echoed from `getFactFamilyCoverage().coverage.contentDigestRef`.
    pub expected_content_digest_ref: String,
    /// Echoed from `getFactFamilyCoverage().coverage.lastCommitSeq`.
    pub expected_coverage_last_commit_seq: f64,
    /// Bounded durable audit reason for this replacement command.
    pub reason: String,
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
pub struct EngineConfiguredObservationSourceOptions {
    /// Open adapter identifier registered by the native composition root.
    pub adapter_id: String,
    /// Configured native data roots understood by that adapter.
    pub roots: Vec<String>,
    /// Durable ingest reason prefix. Defaults to `production_observation`.
    pub reason: Option<String>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineConfiguredObservationOptions {
    /// Complete configured source set. The engine validates and plans this
    /// set as one startup unit before any history scan begins.
    pub sources: Vec<EngineConfiguredObservationSourceOptions>,
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

    /// Current lifecycle status as JSON (`EngineStatusSnapshot`).
    #[napi(getter)]
    pub fn status(&self) -> Result<String> {
        encode_json(&self.inner.status())
    }

    /// Probe the writer and one query worker off the JavaScript thread.
    #[napi]
    pub fn health(&self, signal: Option<AbortSignal>) -> AsyncTask<HealthTask> {
        AsyncTask::with_optional_signal(
            HealthTask {
                engine: Arc::clone(&self.inner),
            },
            signal,
        )
    }

    /// Execute the first typed, read-only Rust query.
    #[napi]
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
    #[napi]
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
    #[napi]
    pub fn wait_for_commit(
        &self,
        env: Env,
        options: EngineCommitWaitOptions,
        signal: Option<AbortSignal>,
    ) -> Result<AsyncBlock<String>> {
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
                .map_err(napi_error)
                .and_then(|value| encode_json(&value))
        })
        .build(&env)
    }

    /// List catalog projects — everything discoverable, complete or explicitly
    /// degraded. Available seconds after `startConfiguredObservation`, without
    /// waiting for history, usage, or full-text search.
    ///
    /// This is a different question from `listHistoryProjects`, which reports
    /// what has been decoded. Both are kept because both are asked.
    #[napi]
    pub fn list_catalog_projects(
        &self,
        options: Option<EngineCatalogPageOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<CatalogProjectsTask> {
        AsyncTask::with_optional_signal(
            CatalogProjectsTask::new(Arc::clone(&self.inner), options),
            signal,
        )
    }

    /// List catalog sessions, optionally within one project. Rows carry the
    /// evidence behind their project association and any competing identity.
    #[napi]
    pub fn list_catalog_sessions(
        &self,
        options: Option<EngineCatalogSessionPageOptions>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<CatalogSessionsTask> {
        AsyncTask::with_optional_signal(
            CatalogSessionsTask::new(Arc::clone(&self.inner), options),
            signal,
        )
    }

    /// Resolve a persisted external reference against the current catalog.
    #[napi]
    pub fn resolve_catalog_entity(
        &self,
        external_ref: String,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<CatalogResolveTask> {
        AsyncTask::with_optional_signal(
            CatalogResolveTask::new(Arc::clone(&self.inner), external_ref),
            signal,
        )
    }

    /// Wait until every configured supervisor has finished starting.
    ///
    /// `startConfiguredObservation` resolves as soon as the catalog commits,
    /// so watchers are still coming up behind it. Callers that need decoded
    /// history await this; callers that only need the library do not.
    #[napi]
    pub fn await_observation_start(
        &self,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<AwaitObservationStartTask> {
        AsyncTask::with_optional_signal(
            AwaitObservationStartTask::new(Arc::clone(&self.inner)),
            signal,
        )
    }

    /// The readiness vector: catalog, history, usage, capabilities, artifacts,
    /// and search, each derived from committed rows.
    #[napi]
    pub fn readiness(&self, signal: Option<AbortSignal>) -> AsyncTask<ReadinessTask> {
        AsyncTask::with_optional_signal(ReadinessTask::new(Arc::clone(&self.inner)), signal)
    }

    /// List canonical projects in Rust-defined activity order. The cursor is
    /// opaque, versioned, and valid only for this query.
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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

    /// Return canonical response-level usage for one project or one verified
    /// session. Supplying `from` and `to` adds the per-day series and the
    /// contributions that no day can own.
    #[napi]
    pub fn get_usage(
        &self,
        options: EngineUsageOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<UsageTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            UsageTask {
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
    #[napi]
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

    /// Replace one fact family's durable evidence after a caller explicitly
    /// echoes the current source, digest, and coverage commit authorization.
    #[napi]
    pub fn replay_fact_family(
        &self,
        options: EngineFactFamilyReplayOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<FactFamilyReplayTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            FactFamilyReplayTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Return durable run-state and current registry-presence evidence. This
    /// intentionally does not probe PIDs or synthesize freshness assessments.
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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

    /// Start the complete configured source set, catalog first: every source
    /// commits its discovered projects and sessions before any watcher begins
    /// a history scan.
    #[napi]
    pub fn start_configured_observation(
        &self,
        options: EngineConfiguredObservationOptions,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<StartConfiguredObservationTask> {
        let cancellation = cancellation_for_signal(signal.as_ref());
        AsyncTask::with_optional_signal(
            StartConfiguredObservationTask {
                engine: Arc::clone(&self.inner),
                options,
                cancellation,
            },
            signal,
        )
    }

    /// Force one running adapter supervisor through common reconciliation.
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
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
    #[napi]
    pub fn complete_query_bootstrap(&self) -> AsyncTask<CompleteQueryBootstrapTask> {
        AsyncTask::new(CompleteQueryBootstrapTask {
            engine: Arc::clone(&self.inner),
        })
    }

    /// Deterministically stop readers, stop the writer, and release ownership.
    #[napi]
    pub fn dispose(&self) -> AsyncTask<DisposeTask> {
        AsyncTask::new(DisposeTask {
            engine: Arc::clone(&self.inner),
        })
    }
}

/// Open the persistent engine on a libuv worker thread.
///
/// The only entry point whose result is a handle rather than JSON, so it is
/// also the only one that still declares its resolved type.
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
        let support_catalog = verified_builtin_support_catalog()
            .map(Arc::new)
            .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))?;
        let registry = verified_builtin_registry(support_catalog)
            .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?;
        let source_pass_pool = crate::source::SharedSourcePassPool::new(
            DEFAULT_SHARED_SOURCE_PASSES,
        )
        .map_err(|_| {
            Error::new(
                Status::GenericFailure,
                "could not initialize bounded source admission".to_string(),
            )
        })?;
        let inner = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: PathBuf::from(&self.options.db_path),
                query_workers,
                owner_label: self.options.owner_label.clone(),
                defer_query_structures: self.options.bootstrap_query_structures.unwrap_or(false),
                source_pass_pool: Some(source_pass_pool),
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
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine.complete_query_bootstrap().map_err(napi_error)?;
        encode_json(&self.engine.status())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct HealthTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl Task for HealthTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        encode_json(&self.engine.health())
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

pub struct UsageTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineUsageOptions,
    cancellation: QueryCancellationToken,
}

pub struct FactFamilyCoverageTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineFactFamilyCoverageOptions,
    cancellation: QueryCancellationToken,
}

pub struct FactFamilyReplayTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineFactFamilyReplayOptions,
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

pub struct StartConfiguredObservationTask {
    engine: Arc<SpaghettiEngineCore>,
    options: EngineConfiguredObservationOptions,
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
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for StartClaudeObservationTask {
    type Output = String;
    type JsValue = String;

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
        encode_json(&self.engine.status())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for StartConfiguredObservationTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        let mut configured = Vec::with_capacity(self.options.sources.len());
        for source in &self.options.sources {
            validate_roots(&source.roots, "startConfiguredObservation")?;
            configured.push(ConfiguredObservationSource::new(
                source.adapter_id.clone(),
                source.roots.iter().map(PathBuf::from).collect(),
                source
                    .reason
                    .clone()
                    .unwrap_or_else(|| "production_observation".to_string()),
            ));
        }
        let outcome = self
            .engine
            .start_configured_observation_cancellable(configured, self.cancellation.clone())
            .map_err(napi_error)?;
        encode_json(&CatalogStartup {
            catalog_projects: outcome.catalog_projects,
            catalog_sessions: outcome.catalog_sessions,
            degraded_sources: outcome.degraded_sources,
            supervisors_started: u32::try_from(outcome.supervisors_started).unwrap_or(u32::MAX),
            history_background: outcome.history_background,
            status: self.engine.status(),
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for RefreshClaudeObservationTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .refresh_observation_supervisor_cancellable(&self.adapter_id, self.cancellation.clone())
            .map_err(napi_error)?;
        // Reconcile catalog membership with the native surface: a created or
        // deleted transcript is a catalog fact, not just a history one.
        self.engine
            .rescan_catalog(Some(&self.adapter_id))
            .map_err(napi_error)?;
        encode_json(&self.engine.status())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for StopClaudeObservationTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .stop_observation_supervisor(&self.adapter_id)
            .map_err(napi_error)?;
        encode_json(&self.engine.status())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for OverviewTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .overview()
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for ChangeReplayTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for HistoryProjectsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for HistorySessionsTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .history_sessions(HistorySessionPageRequest {
                project_id: self.options.project_id.clone(),
                cursor: self.options.cursor.clone(),
                limit: self.options.limit.unwrap_or(DEFAULT_HISTORY_PAGE_LIMIT),
            })
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for SessionDetailsTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .session_details_cancellable(
                SessionDetailsRequest {
                    session_id: self.session_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for MessagesTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for SearchTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TimelineTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for DelegationsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for WorkflowsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for WorkflowDetailsTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .workflow_details_cancellable(
                WorkflowDetailsRequest {
                    workflow_id: self.workflow_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for WorkflowMembersTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for MemoryDocumentsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TaskCollectionsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TasksTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for PlansTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for ToolResultsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for ArtifactsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for SourcesTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for CanonicalStatsTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .canonical_stats_cancellable(self.cancellation.clone())
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for UsageTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        let window = match (self.options.from.clone(), self.options.to.clone()) {
            (Some(from), Some(to)) => Some(UsageWindow { from, to }),
            (None, None) => None,
            _ => {
                return Err(napi_error(EngineError::InvalidQuery(
                    "usage window requires both from and to".to_string(),
                )))
            }
        };
        self.engine
            .usage_cancellable(
                UsageRequest {
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    window,
                },
                self.cancellation.clone(),
            )
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for FactFamilyCoverageTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for FactFamilyReplayTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        validate_roots(&self.options.roots, "replayFactFamily")?;
        let expected_coverage_last_commit_seq = safe_u64_from_js(
            self.options.expected_coverage_last_commit_seq,
            "expectedCoverageLastCommitSeq",
            false,
        )?;
        self.engine
            .replay_fact_family_cancellable(
                FactFamilyReplayCommand {
                    adapter_id: self.options.adapter_id.clone(),
                    configured_roots: self.options.roots.iter().map(PathBuf::from).collect(),
                    project_id: self.options.project_id.clone(),
                    session_id: self.options.session_id.clone(),
                    owner_id: self.options.owner_id.clone(),
                    family: self.options.family.clone(),
                    family_version: self.options.family_version,
                    expected_source_instance_ref: self.options.expected_source_instance_ref.clone(),
                    expected_content_digest_ref: self.options.expected_content_digest_ref.clone(),
                    expected_coverage_last_commit_seq,
                    reason: self.options.reason.clone(),
                },
                self.cancellation.clone(),
            )
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for RuntimeSnapshotTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for RunStateTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .run_state_cancellable(
                RunStateRequest {
                    run_id: self.run_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamsTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamDetailsTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .team_details_cancellable(
                TeamDetailsRequest {
                    team_id: self.team_id.clone(),
                },
                self.cancellation.clone(),
            )
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamInboxesTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl Task for TeamInboxMessagesTask {
    type Output = String;
    type JsValue = String;

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
            .map_err(napi_error)
            .and_then(|value| encode_json(&value))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct DisposeTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl Task for DisposeTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine.shutdown().map_err(napi_error)?;
        encode_json(&self.engine.status())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub(crate) fn napi_error(error: EngineError) -> Error {
    let status = match &error {
        EngineError::InvalidConfig(_)
        | EngineError::InvalidQuery(_)
        | EngineError::InvalidCommit(_) => Status::InvalidArg,
        EngineError::QueryCancelled => Status::Cancelled,
        EngineError::QueryQueueFull => Status::QueueFull,
        EngineError::ShuttingDown => Status::Closing,
        _ => Status::GenericFailure,
    };
    Error::new(status, public_engine_error_message(&error))
}

fn public_engine_error_message(error: &EngineError) -> String {
    match error {
        // Observation details can originate in native paths, source payloads,
        // SQLite diagnostics, or adapter messages. Keep those details inside
        // the engine; the public N-API boundary exposes only the bounded
        // operation label.
        EngineError::Observation { operation, .. } => {
            format!("observation coordinator {operation} failed")
        }
        _ => error.to_string(),
    }
}

fn cancellation_for_signal(signal: Option<&AbortSignal>) -> QueryCancellationToken {
    let cancellation = QueryCancellationToken::default();
    if let Some(signal) = signal {
        let abort_cancellation = cancellation.clone();
        signal.on_abort(move || abort_cancellation.cancel());
    }
    cancellation
}

fn safe_u64_from_js(value: f64, label: &'static str, allow_zero: bool) -> Result<u64> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let minimum = if allow_zero { 0.0 } else { 1.0 };
    if !value.is_finite() || value.fract() != 0.0 || !(minimum..=MAX_SAFE_INTEGER).contains(&value)
    {
        return Err(Error::new(
            Status::InvalidArg,
            format!(
                "{label} must be a {} safe integer",
                if allow_zero {
                    "non-negative"
                } else {
                    "positive"
                }
            ),
        ));
    }
    Ok(value as u64)
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
    use std::sync::Arc;

    use super::{
        public_engine_error_message, verified_builtin_registry, verified_builtin_support_catalog,
    };
    use crate::adapter::{
        verify_support_release_bundle, AdapterId, AgentAdapter, SupportBundleDocument,
        SupportReleaseStatus,
    };
    use crate::claude::ClaudeCodeAdapter;
    use crate::codex::CodexAdapter;
    use crate::engine::EngineError;
    use crate::grok::GrokAdapter;

    #[test]
    fn public_observation_errors_do_not_echo_native_paths_or_payloads() {
        let message = public_engine_error_message(&EngineError::Observation {
            operation: "read source object",
            detail: "/Users/alice/private/session.jsonl: secret payload".to_string(),
        });
        assert_eq!(message, "observation coordinator read source object failed");
        for private in ["/Users/", "alice", "private", "session.jsonl", "secret"] {
            assert!(!message.contains(private));
        }
    }

    #[test]
    fn shipped_registry_has_a_host_probe_for_every_builtin_adapter() {
        let registry = verified_builtin_registry(Arc::new(
            verified_builtin_support_catalog().expect("built-in support catalog is valid"),
        ))
        .expect("built-in adapter registry is valid");
        let root = tempfile::TempDir::new().unwrap();
        for (adapter_id, family) in [
            ("claude-code", "claude-code"),
            ("codex", "codex"),
            ("grok", "grok"),
        ] {
            let probe = registry
                .probe_native_support(
                    &AdapterId::new(adapter_id).unwrap(),
                    &[root.path().to_path_buf()],
                )
                .unwrap()
                .expect("shipped adapter has a registered native probe");
            assert_eq!(probe.family, family);
        }
    }

    #[test]
    fn compiled_adapters_match_their_digest_bound_support_packages() {
        assert_support_binding(
            &ClaudeCodeAdapter::new(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/claude-code/2026-08-21/support-release.json"
            )),
            &[
                SupportBundleDocument::new(
                    "agent-support/claude-code/2026-08-21/ads.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/2026-08-21/ads.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/2026-08-21/source-declarations.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/2026-08-21/source-declarations.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/2026-08-21/scope-programs.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/2026-08-21/scope-programs.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/2026-08-21/evidence.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/2026-08-21/evidence.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/claude-code/2026-08-21/conformance.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/claude-code/2026-08-21/conformance.json"
                    )),
                ),
            ],
            false,
        );
        assert_support_binding(
            &CodexAdapter::new(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/codex/2026-08-15/support-release.json"
            )),
            &[
                SupportBundleDocument::new(
                    "agent-support/codex/2026-08-15/ads.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/2026-08-15/ads.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/2026-08-15/source-declarations.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/2026-08-15/source-declarations.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/2026-08-15/scope-programs.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/2026-08-15/scope-programs.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/2026-08-15/evidence.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/2026-08-15/evidence.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/codex/2026-08-15/conformance.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/codex/2026-08-15/conformance.json"
                    )),
                ),
            ],
            false,
        );
        assert_support_binding(
            &GrokAdapter::new(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/grok/2026-08-15/support-release.json"
            )),
            &[
                SupportBundleDocument::new(
                    "agent-support/grok/2026-08-15/ads.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/2026-08-15/ads.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/2026-08-15/source-declarations.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/2026-08-15/source-declarations.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/2026-08-15/scope-programs.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/2026-08-15/scope-programs.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/2026-08-15/evidence.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/2026-08-15/evidence.json"
                    )),
                ),
                SupportBundleDocument::new(
                    "agent-support/grok/2026-08-15/conformance.json",
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../../agent-support/grok/2026-08-15/conformance.json"
                    )),
                ),
            ],
            false,
        );
    }

    fn assert_support_binding(
        adapter: &dyn AgentAdapter,
        release_json: &[u8],
        documents: &[SupportBundleDocument<'_>],
        expected_selectable: bool,
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
        assert_eq!(release.descriptor().runtime_selectable, expected_selectable);
    }
}
