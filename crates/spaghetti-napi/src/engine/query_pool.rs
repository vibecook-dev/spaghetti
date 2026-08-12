//! Bounded pool of persistent, read-only SQLite query workers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::core::schema;

use super::EngineError;

const QUEUE_DEPTH_PER_WORKER: usize = 16;

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
    pub query_only: bool,
    pub read_only: bool,
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
    pub size_bytes: Option<u64>,
    pub mtime_ns: Option<i64>,
    pub decoder_contract_version: u32,
    pub last_commit_seq: Option<u64>,
    pub state: String,
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
    /// Watermark read at the start of the same SQLite snapshot as `changes`.
    pub at_commit_seq: u64,
    pub oldest_available: Option<ChangeCursor>,
    pub changes: Vec<DurableChange>,
    pub next_cursor: Option<ChangeCursor>,
    pub has_more: bool,
}

enum QueryCommand {
    Overview {
        cancellation_epoch: u64,
        response: Sender<Result<QueryOverview, EngineError>>,
    },
    ReplayChanges {
        cancellation_epoch: u64,
        request: ChangeReplayRequest,
        response: Sender<Result<ChangeReplay, EngineError>>,
    },
    SourceCatalog {
        cancellation_epoch: u64,
        adapter_id: String,
        stable_key: Vec<u8>,
        response: Sender<Result<SourceCatalogSnapshot, EngineError>>,
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

    pub fn replay_changes(
        &self,
        request: ChangeReplayRequest,
    ) -> Result<ChangeReplay, EngineError> {
        validate_replay_request(&request)?;
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(EngineError::ShuttingDown);
        }

        let cancellation_epoch = self.control.cancellation_epoch.load(Ordering::Acquire);
        let (response_tx, response_rx) = bounded(1);
        match self.commands.try_send(QueryCommand::ReplayChanges {
            cancellation_epoch,
            request,
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
        match self.commands.try_send(QueryCommand::SourceCatalog {
            cancellation_epoch,
            adapter_id: adapter_id.to_string(),
            stable_key: stable_key.to_vec(),
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
            QueryCommand::ReplayChanges {
                cancellation_epoch,
                request,
                response,
            } => {
                if is_cancelled(&control, cancellation_epoch) {
                    let _ = response.send(Err(EngineError::QueryCancelled));
                    continue;
                }
                let _in_flight = InFlightGuard::enter(&control.in_flight);
                let result = read_change_replay(&connection, &request).and_then(|replay| {
                    if is_cancelled(&control, cancellation_epoch) {
                        Err(EngineError::QueryCancelled)
                    } else {
                        Ok(replay)
                    }
                });
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

    let overview = QueryOverview {
        schema_version,
        commit_seq: read_commit_seq(&transaction)?,
        projects: count_table(&transaction, "projects")?,
        sessions: count_table(&transaction, "sessions")?,
        messages: count_table(&transaction, "messages")?,
        canonical_sessions: count_table(&transaction, "canonical_sessions")?,
        canonical_messages: count_table(&transaction, "canonical_messages")?,
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

fn read_source_catalog(
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
                       so.decoder_state_version, so.size_bytes, so.mtime_ns,
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
                size_bytes: decode_optional_u64(
                    row.get(14)
                        .map_err(|error| query_sqlite_error("decode source size", error))?,
                    "source size",
                )?,
                mtime_ns: row
                    .get(15)
                    .map_err(|error| query_sqlite_error("decode source mtime", error))?,
                decoder_contract_version: decode_nonnegative_u32(
                    row.get(16).map_err(|error| {
                        query_sqlite_error("decode decoder contract version", error)
                    })?,
                    "decoder contract version",
                )?,
                last_commit_seq: decode_optional_u64(
                    row.get(17)
                        .map_err(|error| query_sqlite_error("decode source commit", error))?,
                    "source commit sequence",
                )?,
                state: row
                    .get(18)
                    .map_err(|error| query_sqlite_error("decode source state", error))?,
            });
        }
        objects
    };

    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish source catalog snapshot", error))?;
    Ok(SourceCatalogSnapshot {
        source_instance_id: Some(source_instance_id),
        adapter_contract_version: Some(adapter_contract_version),
        streams,
        objects,
    })
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
    let oldest_available = transaction
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
        .transpose()?;

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

    let mut changes = {
        let mut statement = transaction
            .prepare(&sql)
            .map_err(|error| query_sqlite_error("prepare change replay", error))?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(arguments.iter()))
            .map_err(|error| query_sqlite_error("execute change replay", error))?;
        let mut changes = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| query_sqlite_error("read durable change", error))?
        {
            let commit_seq: i64 = row
                .get(0)
                .map_err(|error| query_sqlite_error("decode change sequence", error))?;
            let ordinal: i64 = row
                .get(1)
                .map_err(|error| query_sqlite_error("decode change ordinal", error))?;
            let schema_version: i64 = row
                .get(3)
                .map_err(|error| query_sqlite_error("decode change schema version", error))?;
            changes.push(DurableChange {
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
            });
        }
        changes
    };

    transaction
        .commit()
        .map_err(|error| query_sqlite_error("finish change replay snapshot", error))?;

    let has_more = changes.len() > request.limit as usize;
    if has_more {
        changes.truncate(request.limit as usize);
    }
    let next_cursor = changes.last().map(|change| change.cursor);
    Ok(ChangeReplay {
        at_commit_seq: watermark,
        oldest_available,
        changes,
        next_cursor,
        has_more,
    })
}

fn read_committed_watermark(connection: &Connection) -> Result<u64, EngineError> {
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

fn decode_nonnegative_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
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

fn query_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::commit::{
        ChangeEntry, ExpectedSourceCursor, ObservationCommit, SourceInstanceSpec,
        SourceObjectUpdate, SourceStreamSpec,
    };
    use crate::engine::writer::WriterRuntime;
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn commit_request() -> ObservationCommit {
        ObservationCommit {
            source: SourceInstanceSpec {
                adapter_id: "query-fixture".to_string(),
                stable_key: b"root".to_vec(),
                display_name: "Query fixture".to_string(),
                adapter_contract_version: 1,
                discovered_at: 10,
                last_seen_at: 10,
            },
            stream: SourceStreamSpec {
                stream_key: "history".to_string(),
                driver_kind: "append_file".to_string(),
                decoder_key: "fixture".to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: None,
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
        assert_eq!(overview.canonical_sessions, 0);
        assert_eq!(overview.canonical_messages, 0);
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

    #[test]
    fn durable_change_replay_is_ordered_filtered_paginated_and_watermarked() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("replay.db");
        let mut writer = WriterRuntime::start(database.clone()).unwrap();
        let receipt = writer
            .client()
            .commit_observation(commit_request())
            .unwrap();
        let mut pool = QueryPool::start(database, 2).unwrap();
        let client = pool.client();

        let first = client
            .replay_changes(ChangeReplayRequest {
                after: None,
                topics: Vec::new(),
                limit: 1,
            })
            .unwrap();
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
        let mut restarted_queries = QueryPool::start(database, 1).unwrap();
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

        let mut restarted_queries = QueryPool::start(database, 1).unwrap();
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
