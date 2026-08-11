//! Bounded pool of persistent, read-only SQLite query workers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use rusqlite::{Connection, OpenFlags};

use crate::core::schema;

use super::EngineError;

const QUEUE_DEPTH_PER_WORKER: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOverview {
    pub schema_version: u32,
    pub commit_seq: u32,
    pub projects: u32,
    pub sessions: u32,
    pub messages: u32,
    pub query_only: bool,
    pub read_only: bool,
}

enum QueryCommand {
    Overview {
        cancellation_epoch: u64,
        response: Sender<Result<QueryOverview, EngineError>>,
    },
    #[cfg(test)]
    Hold {
        entered: Sender<()>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    ProbeWrite(Sender<bool>),
    Shutdown,
}

struct QueryControl {
    cancellation_epoch: AtomicU64,
    stopping: AtomicBool,
    alive_workers: AtomicUsize,
    in_flight: AtomicUsize,
}

impl QueryControl {
    fn new() -> Self {
        Self {
            cancellation_epoch: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
            alive_workers: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
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

#[derive(Clone)]
pub struct QueryClient {
    commands: Sender<QueryCommand>,
    control: Arc<QueryControl>,
    configured_workers: usize,
}

impl QueryClient {
    pub fn overview(&self) -> Result<QueryOverview, EngineError> {
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(EngineError::ShuttingDown);
        }

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        match self.commands.try_send(QueryCommand::Overview {
            cancellation_epoch,
            response: response_tx,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(EngineError::QueryQueueFull),
            Err(TrySendError::Disconnected(_)) => {
                return Err(EngineError::WorkerUnavailable { worker: "query" });
            }
        }

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

    #[cfg(test)]
    fn hold_worker(&self, entered: Sender<()>, release: Receiver<()>) {
        self.commands
            .send(QueryCommand::Hold { entered, release })
            .unwrap();
    }

    #[cfg(test)]
    fn probe_write_rejected(&self) -> bool {
        let (tx, rx) = bounded(1);
        self.commands.send(QueryCommand::ProbeWrite(tx)).unwrap();
        rx.recv().unwrap()
    }
}

pub struct QueryPool {
    client: QueryClient,
    joins: Vec<JoinHandle<()>>,
}

impl QueryPool {
    pub fn start(database_path: PathBuf, workers: usize) -> Result<Self, EngineError> {
        let capacity = workers.saturating_mul(QUEUE_DEPTH_PER_WORKER).max(1);
        let (command_tx, command_rx) = bounded(capacity);
        let (ready_tx, ready_rx) = bounded(workers);
        let control = Arc::new(QueryControl::new());
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
                let _ = command_tx.send(QueryCommand::Shutdown);
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
            let _ = self.client.commands.send(QueryCommand::Shutdown);
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
    _worker_id: usize,
    database_path: PathBuf,
    commands: Receiver<QueryCommand>,
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

    while let Ok(command) = commands.recv() {
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
            #[cfg(test)]
            QueryCommand::Hold { entered, release } => {
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let _ = entered.send(());
                let _ = release.recv();
            }
            #[cfg(test)]
            QueryCommand::ProbeWrite(response) => {
                let rejected = connection
                    .execute_batch("CREATE TABLE rfc011_query_must_not_write(value INTEGER)")
                    .is_err();
                let _ = response.send(rejected);
            }
            QueryCommand::Shutdown => break,
        }
    }

    control.alive_workers.fetch_sub(1, Ordering::AcqRel);
}

fn is_cancelled(control: &QueryControl, epoch: u64) -> bool {
    control.stopping.load(Ordering::Acquire)
        || control.cancellation_epoch.load(Ordering::Acquire) != epoch
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
    Ok(connection)
}

fn read_overview(connection: &Connection) -> Result<QueryOverview, EngineError> {
    let schema_version = schema::current_schema_version(connection)
        .map_err(|error| EngineError::Sqlite {
            operation: "read schema version",
            detail: error.to_string(),
        })?
        .unwrap_or(0);
    let query_only: i64 = connection
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .map_err(|error| EngineError::Sqlite {
            operation: "verify query_only",
            detail: error.to_string(),
        })?;

    Ok(QueryOverview {
        schema_version,
        commit_seq: read_commit_seq(connection)?,
        projects: count_table(connection, "projects")?,
        sessions: count_table(connection, "sessions")?,
        messages: count_table(connection, "messages")?,
        query_only: query_only != 0,
        // The connection was opened with SQLITE_OPEN_READ_ONLY. The write
        // rejection test below verifies this invariant on the actual handle.
        read_only: true,
    })
}

fn count_table(connection: &Connection, table: &'static str) -> Result<u32, EngineError> {
    let sql = match table {
        "projects" => "SELECT COUNT(*) FROM projects",
        "sessions" => "SELECT COUNT(*) FROM sessions",
        "messages" => "SELECT COUNT(*) FROM messages",
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

fn read_commit_seq(connection: &Connection) -> Result<u32, EngineError> {
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

    let commit_seq: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(commit_seq), 0) FROM ingest_commits",
            [],
            |row| row.get(0),
        )
        .map_err(|error| EngineError::Sqlite {
            operation: "read commit watermark",
            detail: error.to_string(),
        })?;
    Ok(u32::try_from(commit_seq).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::writer::WriterRuntime;
    use rusqlite::Connection;
    use tempfile::tempdir;

    #[test]
    fn overview_is_typed_read_only_and_does_not_change_database_content() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("query.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database.clone(), 2).unwrap();
        let client = pool.client();

        let probe = Connection::open(&database).unwrap();
        let before: i64 = probe
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();
        let overview = client.overview().unwrap();
        let after: i64 = probe
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();

        assert_eq!(overview.schema_version, schema::SCHEMA_VERSION);
        assert_eq!(overview.projects, 0);
        assert_eq!(overview.sessions, 0);
        assert_eq!(overview.messages, 0);
        assert!(overview.query_only && overview.read_only);
        assert_eq!(before, after, "a query must not advance database content");
        assert!(client.probe_write_rejected());

        pool.shutdown().unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn cancellation_epoch_rejects_queued_work_but_accepts_new_work() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("cancel.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let mut pool = QueryPool::start(database, 1).unwrap();
        let client = pool.client();

        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        client.hold_worker(entered_tx, release_rx);
        entered_rx.recv().unwrap();

        let queued_client = client.clone();
        let queued = thread::spawn(move || queued_client.overview());
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
}
