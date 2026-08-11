//! N-API host adapter for the library-first persistent engine.

use std::path::PathBuf;
use std::sync::Arc;

use napi::bindgen_prelude::{AbortSignal, AsyncTask, Env, Error, Result, Status, Task};
use napi_derive::napi;

use crate::engine::{
    EngineHealthSnapshot, EngineOptions, EngineOverview, EngineStatusSnapshot, OwnerMetadata,
    SpaghettiEngineCore,
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
pub struct EngineStatus {
    pub state: String,
    pub database_path: String,
    pub accepting_queries: bool,
    pub writer_alive: bool,
    pub configured_query_workers: u32,
    pub alive_query_workers: u32,
    pub in_flight_queries: u32,
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
    pub projects: u32,
    pub sessions: u32,
    pub messages: u32,
    pub writer_data_version: u32,
    pub journal_mode: String,
    pub query_only: bool,
    pub read_only: bool,
}

impl From<EngineOverview> for EngineOverviewResult {
    fn from(value: EngineOverview) -> Self {
        Self {
            schema_version: value.schema_version,
            commit_seq: value.commit_seq as f64,
            projects: value.projects,
            sessions: value.sessions,
            messages: value.messages,
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
