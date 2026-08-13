//! Long-lived engine writer connection.
//!
//! Keeping the `rusqlite::Connection` inside its dedicated thread makes
//! single-writer ownership structural and keeps N-API objects `Send` without
//! moving SQLite handles across runtimes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use rusqlite::Connection;

use crate::adapter::FactBatch;
use crate::core::schema;

use super::commit::{
    self, ChangeLogRetentionPolicy, ChangeLogRetentionSnapshot, CommitReceipt, ObservationCommit,
};
use super::projection;
use super::EngineError;

const MIN_DISK_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_DISK_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const WRITER_STATEMENT_CACHE_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub struct WriterHealth {
    pub data_version: u32,
    pub journal_mode: String,
}

enum WriterCommand {
    Health(Sender<Result<WriterHealth, EngineError>>),
    ReserveSourceInstance {
        source: Box<commit::SourceInstanceSpec>,
        response: Sender<Result<u64, EngineError>>,
    },
    Commit {
        request: Box<ObservationCommit>,
        response: Sender<Result<CommitReceipt, EngineError>>,
    },
    CommitFacts {
        request: Box<ObservationCommit>,
        batch: Box<FactBatch>,
        response: Sender<Result<CommitReceipt, EngineError>>,
    },
    MaintainChangeLog {
        policy: ChangeLogRetentionPolicy,
        now_ms: i64,
        response: Sender<Result<ChangeLogRetentionSnapshot, EngineError>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct WriterClient {
    commands: Sender<WriterCommand>,
    alive: Arc<AtomicBool>,
}

impl WriterClient {
    pub fn health(&self) -> Result<WriterHealth, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::Health(response_tx))
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn reserve_source_instance(
        &self,
        source: commit::SourceInstanceSpec,
    ) -> Result<u64, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::ReserveSourceInstance {
                source: Box::new(source),
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub fn commit_observation(
        &self,
        request: ObservationCommit,
    ) -> Result<CommitReceipt, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::Commit {
                request: Box::new(request),
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub fn commit_facts(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<CommitReceipt, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::CommitFacts {
                request: Box::new(request),
                batch: Box::new(batch),
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub fn maintain_change_log(
        &self,
        policy: ChangeLogRetentionPolicy,
        now_ms: i64,
    ) -> Result<ChangeLogRetentionSnapshot, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::MaintainChangeLog {
                policy,
                now_ms,
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }
}

pub struct WriterRuntime {
    client: WriterClient,
    join: Option<JoinHandle<()>>,
}

impl WriterRuntime {
    pub fn start(database_path: PathBuf) -> Result<Self, EngineError> {
        let (command_tx, command_rx) = bounded(64);
        let (ready_tx, ready_rx) = bounded(1);
        let alive = Arc::new(AtomicBool::new(false));
        let thread_alive = Arc::clone(&alive);

        let join = thread::Builder::new()
            .name("spaghetti-writer".to_string())
            .spawn(move || writer_thread(database_path, command_rx, ready_tx, thread_alive))
            .map_err(|error| EngineError::WorkerStart {
                worker: "writer",
                detail: error.to_string(),
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                client: WriterClient {
                    commands: command_tx,
                    alive,
                },
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(EngineError::WorkerStart {
                    worker: "writer",
                    detail: "writer exited before reporting readiness".to_string(),
                })
            }
        }
    }

    pub fn client(&self) -> WriterClient {
        self.client.clone()
    }

    pub fn shutdown(&mut self) -> Result<(), EngineError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        let _ = self.client.commands.send(WriterCommand::Shutdown);
        join.join()
            .map_err(|_| EngineError::WorkerPanic { worker: "writer" })
    }
}

impl Drop for WriterRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn writer_thread(
    database_path: PathBuf,
    commands: Receiver<WriterCommand>,
    ready: Sender<Result<(), EngineError>>,
    alive: Arc<AtomicBool>,
) {
    let mut connection = match open_writer(&database_path) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    alive.store(true, Ordering::Release);
    if ready.send(Ok(())).is_err() {
        alive.store(false, Ordering::Release);
        return;
    }

    while let Ok(command) = commands.recv() {
        match command {
            WriterCommand::Health(response) => {
                let _ = response.send(read_health(&connection));
            }
            WriterCommand::ReserveSourceInstance { source, response } => {
                let result = ensure_disk_reserve(&database_path)
                    .and_then(|()| commit::reserve_source_instance(&mut connection, &source));
                let _ = response.send(result);
            }
            WriterCommand::Commit { request, response } => {
                let result = ensure_disk_reserve(&database_path)
                    .and_then(|()| commit::apply_observation_commit(&mut connection, &request));
                let _ = response.send(result);
            }
            WriterCommand::CommitFacts {
                request,
                batch,
                response,
            } => {
                let result = ensure_disk_reserve(&database_path).and_then(|()| {
                    projection::apply_fact_observation_commit(&mut connection, &request, &batch)
                });
                let _ = response.send(result);
            }
            WriterCommand::MaintainChangeLog {
                policy,
                now_ms,
                response,
            } => {
                let _ = response.send(commit::maintain_change_log(&mut connection, policy, now_ms));
            }
            WriterCommand::Shutdown => break,
        }
    }

    alive.store(false, Ordering::Release);
}

fn ensure_disk_reserve(database_path: &Path) -> Result<(), EngineError> {
    let filesystem_path = database_path.parent().unwrap_or(database_path);
    let stats = rustix::fs::statvfs(filesystem_path).map_err(|error| EngineError::Sqlite {
        operation: "inspect observation database free space",
        detail: error.to_string(),
    })?;
    let fragment_bytes = stats.f_frsize.max(stats.f_bsize).max(1);
    let available_bytes = stats.f_bavail.saturating_mul(fragment_bytes);
    let total_bytes = stats.f_blocks.saturating_mul(fragment_bytes);
    let reserve_bytes = disk_reserve_bytes(total_bytes);
    if available_bytes < reserve_bytes {
        return Err(EngineError::InsufficientDiskSpace {
            database_path: database_path.to_path_buf(),
            available_bytes,
            reserve_bytes,
        });
    }
    Ok(())
}

fn disk_reserve_bytes(total_bytes: u64) -> u64 {
    (total_bytes / 50).clamp(MIN_DISK_RESERVE_BYTES, MAX_DISK_RESERVE_BYTES)
}

fn open_writer(database_path: &PathBuf) -> Result<Connection, EngineError> {
    let connection = Connection::open(database_path).map_err(|error| EngineError::Sqlite {
        operation: "open writer connection",
        detail: error.to_string(),
    })?;
    super::local_permissions::restrict_owner_file(database_path).map_err(|error| {
        EngineError::Sqlite {
            operation: "restrict writer database permissions",
            detail: error.to_string(),
        }
    })?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| EngineError::Sqlite {
            operation: "configure writer busy timeout",
            detail: error.to_string(),
        })?;
    schema::set_pragmas(&connection).map_err(|error| EngineError::Sqlite {
        operation: "configure writer pragmas",
        detail: error.to_string(),
    })?;
    schema::initialize_schema(&connection).map_err(|error| EngineError::Sqlite {
        operation: "initialize schema",
        detail: error.to_string(),
    })?;
    // Let SQLite refresh bounded planner statistics for tables that need it.
    // The 0x10000 bit asks a newly opened long-lived connection to consider
    // every table once; SQLite's optimize pragma decides whether ANALYZE work
    // is actually necessary and limits the analysis scope.
    connection
        .execute_batch("PRAGMA optimize=0x10002")
        .map_err(|error| EngineError::Sqlite {
            operation: "optimize writer query planner",
            detail: error.to_string(),
        })?;
    // Projection SQL is deliberately stable across commits. Keep the hot
    // statements compiled on this long-lived sole-writer connection instead
    // of paying sqlite3_prepare_v2 for every fact.
    connection.set_prepared_statement_cache_capacity(WRITER_STATEMENT_CACHE_CAPACITY);
    super::local_permissions::restrict_sqlite_files(database_path).map_err(|error| {
        EngineError::Sqlite {
            operation: "restrict writer SQLite sidecar permissions",
            detail: error.to_string(),
        }
    })?;
    Ok(connection)
}

fn read_health(connection: &Connection) -> Result<WriterHealth, EngineError> {
    let data_version: i64 = connection
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .map_err(|error| EngineError::Sqlite {
            operation: "read writer data_version",
            detail: error.to_string(),
        })?;
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(|error| EngineError::Sqlite {
            operation: "read writer journal_mode",
            detail: error.to_string(),
        })?;
    Ok(WriterHealth {
        data_version: u32::try_from(data_version).unwrap_or(u32::MAX),
        journal_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writer_connection_stays_alive_until_shutdown() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("writer.db");
        let mut runtime = WriterRuntime::start(database).unwrap();
        let client = runtime.client();

        let health = client.health().unwrap();
        assert!(client.is_alive());
        assert_eq!(health.journal_mode, "wal");

        runtime.shutdown().unwrap();
        assert!(!client.is_alive());
        assert!(matches!(
            client.health(),
            Err(EngineError::WorkerUnavailable { worker: "writer" })
        ));
    }

    #[test]
    fn disk_reserve_is_bounded_and_keeps_two_percent_on_normal_volumes() {
        assert_eq!(
            disk_reserve_bytes(10 * 1024 * 1024 * 1024),
            MIN_DISK_RESERVE_BYTES
        );
        assert_eq!(
            disk_reserve_bytes(100 * 1024 * 1024 * 1024),
            2 * 1024 * 1024 * 1024
        );
        assert_eq!(disk_reserve_bytes(u64::MAX), MAX_DISK_RESERVE_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn writer_database_is_restricted_to_the_current_user() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("permissions.db");
        let mut runtime = WriterRuntime::start(database.clone()).unwrap();
        assert_eq!(
            std::fs::metadata(&database).unwrap().permissions().mode() & 0o777,
            0o600
        );
        runtime.shutdown().unwrap();
    }
}
