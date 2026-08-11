//! Persistent observation/query engine lifecycle (RFC 011).
//!
//! This module is deliberately free of N-API types. The Node binding in
//! `napi_engine` is one host adapter; a future daemon or embedded desktop host
//! can own the same `SpaghettiEngineCore` and receive identical semantics.

mod commit;
mod owner_lock;
mod presence_projection;
mod projection;
mod query_pool;
mod task_projection;
mod team_projection;
mod writer;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::adapter::FactBatch;
use commit::{CommitReceipt, ObservationCommit};
use owner_lock::DatabaseOwnerLock;
pub use owner_lock::OwnerMetadata;
pub use query_pool::{
    ChangeCursor, ChangeReplay, ChangeReplayRequest, DurableChange, QueryOverview,
};
use query_pool::{QueryClient, QueryPool};
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
    pub projects: u32,
    pub sessions: u32,
    pub messages: u32,
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
    lifecycle: Mutex<Lifecycle>,
    stopped: Condvar,
}

impl SpaghettiEngineCore {
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

        EngineStatusSnapshot {
            state: lifecycle.phase.as_str().to_string(),
            database_path: self.database_path.to_string_lossy().into_owned(),
            accepting_queries: running,
            writer_alive,
            configured_query_workers: usize_to_u32(self.query_workers),
            alive_query_workers: usize_to_u32(alive_query_workers),
            in_flight_queries: usize_to_u32(in_flight_queries),
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
            Ok(()) if worker_counts_healthy => EngineHealthSnapshot {
                status,
                healthy: true,
                detail: None,
            },
            Ok(()) => EngineHealthSnapshot {
                status,
                healthy: false,
                detail: Some("one or more persistent workers are unavailable".to_string()),
            },
            Err(error) => EngineHealthSnapshot {
                status,
                healthy: false,
                detail: Some(error.to_string()),
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
            writer_data_version: writer_health.data_version,
            journal_mode: writer_health.journal_mode,
            query_only: query.query_only,
            read_only: query.read_only,
        })
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

    pub fn cancel_pending_queries(&self) -> Result<u64, EngineError> {
        let (_, queries) = self.clients()?;
        Ok(queries.cancel_pending())
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
        if let Err(error) = runtime.queries.shutdown() {
            first_error = Some(error);
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

    fn lock_lifecycle(&self) -> MutexGuard<'_, Lifecycle> {
        self.lifecycle
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
        assert_eq!(status.owner.unwrap().owner_label, "engine-test");

        let health = engine.health();
        assert!(health.healthy, "{:?}", health.detail);
        let overview = engine.overview().unwrap();
        assert_eq!(overview.schema_version, crate::core::schema::SCHEMA_VERSION);
        assert!(overview.query_only && overview.read_only);

        engine.shutdown().unwrap();
        let stopped = engine.status();
        assert_eq!(stopped.state, "stopped");
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
