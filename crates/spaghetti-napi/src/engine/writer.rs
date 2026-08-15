//! Long-lived engine writer connection.
//!
//! Keeping the `rusqlite::Connection` inside its dedicated thread makes
//! single-writer ownership structural and keeps N-API objects `Send` without
//! moving SQLite handles across runtimes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TryRecvError};
use rusqlite::{Connection, TransactionBehavior};

use crate::adapter::FactBatch;
use crate::core::schema;

use super::commit::{
    self, ChangeLogRetentionPolicy, ChangeLogRetentionSnapshot, CommitDetail, CommitHook,
    CommitReceipt, CommitStage, ObservationCommit,
};
use super::performance::{
    atomic_max, atomic_saturating_add, duration_ns, CheckpointPerformanceSnapshot,
    LatencyHistogram, NamedLatencySnapshot, WriterPerformanceSnapshot,
};
use super::projection;
use super::query_pool::{read_source_catalog, SourceCatalogSnapshot};
use super::EngineError;

const MIN_DISK_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_DISK_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const WRITER_STATEMENT_CACHE_CAPACITY: usize = 256;
const WRITER_QUEUE_CAPACITY: usize = 256;
const MAX_PHYSICAL_COMMIT_GROUP: usize = 8;
const BOOTSTRAP_PHYSICAL_COMMIT_GROUP: usize = 256;
const BOOTSTRAP_PHYSICAL_GROUP_MAX_FACTS: u64 = 65_536;
const GROUP_COMMIT_COLLECTION_WINDOW: Duration = Duration::from_micros(100);
const BOOTSTRAP_GROUP_COMMIT_COLLECTION_WINDOW: Duration = Duration::from_millis(5);
const WAL_CHECKPOINT_TARGET_BYTES: u64 = 32 * 1024 * 1024;
const BOOTSTRAP_WAL_CHECKPOINT_TARGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const WAL_CHECKPOINT_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const WRITER_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
    SourceCatalog {
        adapter_id: String,
        stable_key: Vec<u8>,
        response: Sender<Result<SourceCatalogSnapshot, EngineError>>,
    },
    BeginQueryBootstrap {
        response: Sender<Result<bool, EngineError>>,
    },
    FinalizeQueryBootstrap {
        response: Sender<Result<Option<u64>, EngineError>>,
    },
    Commit {
        request: Box<ObservationCommit>,
        queued_at: Instant,
        response: Sender<Result<CommitReceipt, EngineError>>,
    },
    CommitFacts {
        request: Box<ObservationCommit>,
        batch: Box<FactBatch>,
        queued_at: Instant,
        response: Sender<Result<CommitReceipt, EngineError>>,
    },
    MaintainChangeLog {
        policy: ChangeLogRetentionPolicy,
        now_ms: i64,
        response: Sender<Result<ChangeLogRetentionSnapshot, EngineError>>,
    },
    Checkpoint {
        response: Sender<Result<CheckpointOutcome, EngineError>>,
    },
    Shutdown {
        response: Sender<Result<CheckpointOutcome, EngineError>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CheckpointOutcome {
    pub busy: bool,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
    pub remaining_frames: u64,
}

#[derive(Clone)]
pub struct WriterClient {
    commands: Sender<WriterCommand>,
    alive: Arc<AtomicBool>,
    telemetry: Arc<WriterTelemetry>,
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

    pub(crate) fn source_catalog(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
    ) -> Result<SourceCatalogSnapshot, EngineError> {
        if adapter_id.trim().is_empty() || stable_key.is_empty() {
            return Err(EngineError::InvalidQuery(
                "source catalog requires a non-empty adapter id and stable key".to_string(),
            ));
        }
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::SourceCatalog {
                adapter_id: adapter_id.to_string(),
                stable_key: stable_key.to_vec(),
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub(crate) fn begin_query_bootstrap(&self) -> Result<bool, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::BeginQueryBootstrap {
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub(crate) fn finalize_query_bootstrap(&self) -> Result<Option<u64>, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::FinalizeQueryBootstrap {
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
        self.submit_observation(request)?
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub fn commit_facts(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<CommitReceipt, EngineError> {
        self.submit_facts(request, batch)?
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    pub(crate) fn submit_observation(
        &self,
        request: ObservationCommit,
    ) -> Result<Receiver<Result<CommitReceipt, EngineError>>, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        let queued_at = self.observe_commit_enqueue();
        self.commands
            .send(WriterCommand::Commit {
                request: Box::new(request),
                queued_at,
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        Ok(response_rx)
    }

    pub(crate) fn submit_facts(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<Receiver<Result<CommitReceipt, EngineError>>, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        let queued_at = self.observe_commit_enqueue();
        self.commands
            .send(WriterCommand::CommitFacts {
                request: Box::new(request),
                batch: Box::new(batch),
                queued_at,
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        Ok(response_rx)
    }

    pub fn performance_snapshot(&self) -> WriterPerformanceSnapshot {
        self.telemetry.snapshot(self.commands.len())
    }

    pub(crate) fn record_changes_published(&self, count: u32) {
        atomic_saturating_add(&self.telemetry.changes_published, u64::from(count));
    }

    pub(crate) fn checkpoint(&self) -> Result<CheckpointOutcome, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::Checkpoint {
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }

    fn observe_commit_enqueue(&self) -> Instant {
        self.telemetry
            .observe_queue_depth(self.commands.len().saturating_add(1));
        Instant::now()
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
        let (command_tx, command_rx) = bounded(WRITER_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = bounded(1);
        let alive = Arc::new(AtomicBool::new(false));
        let thread_alive = Arc::clone(&alive);
        let telemetry = Arc::new(WriterTelemetry::new());
        let thread_telemetry = Arc::clone(&telemetry);

        let join = thread::Builder::new()
            .name("spaghetti-writer".to_string())
            .spawn(move || {
                writer_thread(
                    database_path,
                    command_rx,
                    ready_tx,
                    thread_alive,
                    thread_telemetry,
                )
            })
            .map_err(|error| EngineError::WorkerStart {
                worker: "writer",
                detail: error.to_string(),
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                client: WriterClient {
                    commands: command_tx,
                    alive,
                    telemetry,
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
        let (response_tx, response_rx) = bounded(1);
        let checkpoint = if self
            .client
            .commands
            .send(WriterCommand::Shutdown {
                response: response_tx,
            })
            .is_ok()
        {
            response_rx
                .recv()
                .unwrap_or(Err(EngineError::WorkerUnavailable { worker: "writer" }))
        } else {
            Err(EngineError::WorkerUnavailable { worker: "writer" })
        };
        join.join()
            .map_err(|_| EngineError::WorkerPanic { worker: "writer" })?;
        checkpoint.map(|_| ())
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
    telemetry: Arc<WriterTelemetry>,
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
    let mut checkpoints = CheckpointController::new(&database_path);
    let mut pending = None;
    let mut bootstrap_active = query_bootstrap_active(&connection).unwrap_or(false);

    'writer: loop {
        let command = match pending.take() {
            Some(command) => command,
            None => match commands.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
        };
        if is_commit_command(&command) {
            let (group, leftover) = collect_commit_group(&commands, command, bootstrap_active);
            pending = leftover;
            process_commit_group(
                &mut connection,
                &database_path,
                &telemetry,
                &mut checkpoints,
                group,
                true,
                bootstrap_active,
            );
            continue;
        }
        match command {
            WriterCommand::Health(response) => {
                let _ = response.send(read_health(&connection));
            }
            WriterCommand::ReserveSourceInstance { source, response } => {
                let result = ensure_disk_reserve(&database_path)
                    .and_then(|()| commit::reserve_source_instance(&mut connection, &source));
                let _ = response.send(result);
            }
            WriterCommand::SourceCatalog {
                adapter_id,
                stable_key,
                response,
            } => {
                let _ = response.send(read_source_catalog(&connection, &adapter_id, &stable_key));
            }
            WriterCommand::BeginQueryBootstrap { response } => {
                let result = ensure_disk_reserve(&database_path).and_then(|()| {
                    let started =
                        schema::begin_query_bootstrap(&mut connection).map_err(|error| {
                            EngineError::Sqlite {
                                operation: "begin durable query bootstrap",
                                detail: error.to_string(),
                            }
                        })?;
                    if started {
                        schema::set_bootstrap_ingest_pragmas(&connection).map_err(|error| {
                            EngineError::Sqlite {
                                operation: "configure bootstrap ingest pragmas",
                                detail: error.to_string(),
                            }
                        })?;
                    }
                    bootstrap_active = started || query_bootstrap_active(&connection)?;
                    Ok(started)
                });
                let _ = response.send(result);
            }
            WriterCommand::FinalizeQueryBootstrap { response } => {
                let started = Instant::now();
                let skip = super::ingest_profile::IngestProfileSkip::current();
                let result = query_bootstrap_active(&connection).and_then(|active| {
                    if !active {
                        bootstrap_active = false;
                        return Ok(None);
                    }
                    ensure_disk_reserve(&database_path)?;
                    if !skip.checkpoints && !skip.finalize {
                        checkpoints.checkpoint(&connection, &telemetry, true)?;
                    }
                    let watermark = finalize_query_bootstrap_connection(&mut connection)?;
                    if !skip.checkpoints && !skip.finalize {
                        checkpoints.checkpoint(&connection, &telemetry, true)?;
                    }
                    bootstrap_active = false;
                    Ok(watermark)
                });
                telemetry.bootstrap_finalize.record(started.elapsed());
                let _ = response.send(result);
            }
            WriterCommand::Commit { .. } | WriterCommand::CommitFacts { .. } => {
                unreachable!("commit commands are grouped before dispatch")
            }
            WriterCommand::MaintainChangeLog {
                policy,
                now_ms,
                response,
            } => {
                let _ = response.send(commit::maintain_change_log(&mut connection, policy, now_ms));
            }
            WriterCommand::Checkpoint { response } => {
                let _ = response.send(checkpoints.checkpoint(&connection, &telemetry, false));
            }
            WriterCommand::Shutdown { response } => {
                let _ = response.send(checkpoints.checkpoint(
                    &connection,
                    &telemetry,
                    bootstrap_active,
                ));
                break 'writer;
            }
        }
    }

    alive.store(false, Ordering::Release);
}

fn is_commit_command(command: &WriterCommand) -> bool {
    matches!(
        command,
        WriterCommand::Commit { .. } | WriterCommand::CommitFacts { .. }
    )
}

fn collect_commit_group(
    commands: &Receiver<WriterCommand>,
    first: WriterCommand,
    bootstrap_active: bool,
) -> (Vec<WriterCommand>, Option<WriterCommand>) {
    let max_group = if bootstrap_active {
        BOOTSTRAP_PHYSICAL_COMMIT_GROUP
    } else {
        MAX_PHYSICAL_COMMIT_GROUP
    };
    let max_facts = if bootstrap_active {
        BOOTSTRAP_PHYSICAL_GROUP_MAX_FACTS
    } else {
        u64::MAX
    };
    let collect_window = if bootstrap_active {
        BOOTSTRAP_GROUP_COMMIT_COLLECTION_WINDOW
    } else {
        GROUP_COMMIT_COLLECTION_WINDOW
    };
    let mut group = vec![first];
    let mut facts = commit_fact_count(group.last().expect("group starts with one commit"));
    let mut leftover = None;
    let mut waited = false;
    loop {
        if group.len() >= max_group {
            break;
        }
        match commands.try_recv() {
            Ok(next) if is_commit_command(&next) => {
                let next_facts = commit_fact_count(&next);
                if facts.saturating_add(next_facts) > max_facts {
                    leftover = Some(next);
                    break;
                }
                facts = facts.saturating_add(next_facts);
                group.push(next);
            }
            Ok(next) => {
                leftover = Some(next);
                break;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {
                let wait = bootstrap_active || (group.len() == 1 && !waited);
                if !wait {
                    break;
                }
                waited = true;
                match commands.recv_timeout(collect_window) {
                    Ok(next) if is_commit_command(&next) => {
                        let next_facts = commit_fact_count(&next);
                        if facts.saturating_add(next_facts) > max_facts {
                            leftover = Some(next);
                            break;
                        }
                        facts = facts.saturating_add(next_facts);
                        group.push(next);
                        if bootstrap_active {
                            waited = false;
                        }
                    }
                    Ok(next) => {
                        leftover = Some(next);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }
        }
    }
    (group, leftover)
}

fn process_commit_group(
    connection: &mut Connection,
    database_path: &Path,
    telemetry: &WriterTelemetry,
    checkpoints: &mut CheckpointController,
    commands: Vec<WriterCommand>,
    record_queue_wait: bool,
    bootstrap_active: bool,
) {
    debug_assert!(!commands.is_empty());
    debug_assert!(commands.iter().all(is_commit_command));

    if record_queue_wait {
        for command in &commands {
            telemetry
                .queue_wait
                .record(commit_queued_at(command).elapsed());
        }
    }

    let reserve_started = Instant::now();
    let reserve = ensure_disk_reserve(database_path);
    let reserve_elapsed = reserve_started.elapsed();
    for _ in &commands {
        telemetry.disk_reserve.record(reserve_elapsed);
    }
    if let Err(error) = reserve {
        if commands.len() > 1 {
            for command in commands {
                process_commit_group(
                    connection,
                    database_path,
                    telemetry,
                    checkpoints,
                    vec![command],
                    false,
                    bootstrap_active,
                );
            }
        } else {
            finish_failed_commit(telemetry, commands.into_iter().next().unwrap(), error);
        }
        return;
    }

    let physical_started = Instant::now();
    let persist_public_changes = !bootstrap_active;
    let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(transaction) => transaction,
        Err(error) => {
            let detail = error.to_string();
            let elapsed = physical_started.elapsed();
            telemetry.physical_transaction.record(elapsed);
            let share = elapsed / u32::try_from(commands.len()).unwrap_or(1);
            for command in commands {
                telemetry.writer_total.record(share);
                finish_failed_commit(
                    telemetry,
                    command,
                    EngineError::Sqlite {
                        operation: "begin grouped ingest transaction",
                        detail: detail.clone(),
                    },
                );
            }
            return;
        }
    };

    let mut hooks = Vec::with_capacity(commands.len());
    let mut receipts = Vec::with_capacity(commands.len());
    let mut row_ranges = Vec::with_capacity(commands.len());
    let mut logical_elapsed = Vec::with_capacity(commands.len());
    let mut logical_error = None;
    for command in &commands {
        hooks.push(CommitTimingHook::new());
        let hook = hooks.last().expect("the commit timing hook was just added");
        let rows_before = sqlite_total_changes(&transaction);
        let started = Instant::now();
        let result = match command {
            WriterCommand::Commit { request, .. } => {
                commit::apply_observation_commit_in_transaction(
                    &transaction,
                    request,
                    hook,
                    persist_public_changes,
                )
            }
            WriterCommand::CommitFacts { request, batch, .. } => {
                projection::apply_fact_observation_commit_in_transaction(
                    &transaction,
                    request,
                    batch,
                    hook,
                    persist_public_changes,
                )
            }
            _ => unreachable!("non-commit command entered a physical commit group"),
        };
        logical_elapsed.push(started.elapsed());
        row_ranges.push((rows_before, sqlite_total_changes(&transaction)));
        match result {
            Ok(receipt) => receipts.push(receipt),
            Err(error) => {
                logical_error = Some(error);
                break;
            }
        }
    }

    if let Some(error) = logical_error {
        let _ = transaction.rollback();
        telemetry
            .physical_transaction
            .record(physical_started.elapsed());
        if commands.len() > 1 {
            for command in commands {
                process_commit_group(
                    connection,
                    database_path,
                    telemetry,
                    checkpoints,
                    vec![command],
                    false,
                    bootstrap_active,
                );
            }
        } else {
            let elapsed = logical_elapsed.into_iter().next().unwrap_or_default();
            telemetry.writer_total.record(elapsed);
            finish_failed_commit(telemetry, commands.into_iter().next().unwrap(), error);
        }
        return;
    }

    let sqlite_commit_started = Instant::now();
    if let Err(error) = transaction.commit() {
        let sqlite_commit_elapsed = sqlite_commit_started.elapsed();
        telemetry
            .physical_sqlite_commit
            .record(sqlite_commit_elapsed);
        telemetry
            .physical_transaction
            .record(physical_started.elapsed());
        let detail = error.to_string();
        let share = sqlite_commit_elapsed / u32::try_from(commands.len()).unwrap_or(1);
        for (command, elapsed) in commands.into_iter().zip(logical_elapsed) {
            telemetry.writer_total.record(elapsed + share);
            finish_failed_commit(
                telemetry,
                command,
                EngineError::Sqlite {
                    operation: "commit grouped ingest transaction",
                    detail: detail.clone(),
                },
            );
        }
        return;
    }

    let sqlite_commit_elapsed = sqlite_commit_started.elapsed();
    let physical_elapsed = physical_started.elapsed();
    telemetry
        .physical_sqlite_commit
        .record(sqlite_commit_elapsed);
    telemetry.physical_transaction.record(physical_elapsed);
    let commit_share = sqlite_commit_elapsed / u32::try_from(commands.len()).unwrap_or(1);
    for ((((command, receipt), hook), (rows_before, rows_after)), elapsed) in commands
        .into_iter()
        .zip(receipts)
        .zip(hooks)
        .zip(row_ranges)
        .zip(logical_elapsed)
    {
        let result = commit::complete_observation_commit(&hook).map(|()| receipt);
        hook.attribute_group_commit(commit_share);
        telemetry.writer_total.record(elapsed + commit_share);
        telemetry.record_commit_result(
            &result,
            commit_fact_count(&command),
            rows_before,
            rows_after,
            &hook,
            matches!(command, WriterCommand::CommitFacts { .. }),
        );
        atomic_saturating_add(&telemetry.commit_attempts, 1);
        send_commit_result(command, result);
    }
    checkpoints.maybe_checkpoint(connection, telemetry, bootstrap_active);
}

fn commit_queued_at(command: &WriterCommand) -> Instant {
    match command {
        WriterCommand::Commit { queued_at, .. } | WriterCommand::CommitFacts { queued_at, .. } => {
            *queued_at
        }
        _ => unreachable!("non-commit command has no queue timestamp"),
    }
}

fn commit_fact_count(command: &WriterCommand) -> u64 {
    match command {
        WriterCommand::Commit { request, .. } => u64::from(request.fact_count),
        WriterCommand::CommitFacts { batch, .. } => {
            u64::try_from(batch.facts().len()).unwrap_or(u64::MAX)
        }
        _ => unreachable!("non-commit command has no fact count"),
    }
}

fn finish_failed_commit(telemetry: &WriterTelemetry, command: WriterCommand, error: EngineError) {
    atomic_saturating_add(&telemetry.commit_attempts, 1);
    atomic_saturating_add(&telemetry.failed, 1);
    send_commit_result(command, Err(error));
}

fn send_commit_result(command: WriterCommand, result: Result<CommitReceipt, EngineError>) {
    match command {
        WriterCommand::Commit { response, .. } | WriterCommand::CommitFacts { response, .. } => {
            let _ = response.send(result);
        }
        _ => unreachable!("non-commit command cannot receive a commit result"),
    }
}

struct CheckpointController {
    wal_path: PathBuf,
    last_attempt: Option<Instant>,
}

impl CheckpointController {
    fn new(database_path: &Path) -> Self {
        let mut wal_path = database_path.as_os_str().to_os_string();
        wal_path.push("-wal");
        Self {
            wal_path: PathBuf::from(wal_path),
            last_attempt: None,
        }
    }

    fn maybe_checkpoint(
        &mut self,
        connection: &Connection,
        telemetry: &WriterTelemetry,
        bootstrap_active: bool,
    ) {
        let wal_bytes = std::fs::metadata(&self.wal_path)
            .map(|metadata| metadata.len())
            .unwrap_or_default();
        let target = if bootstrap_active {
            BOOTSTRAP_WAL_CHECKPOINT_TARGET_BYTES
        } else {
            WAL_CHECKPOINT_TARGET_BYTES
        };
        if super::ingest_profile::IngestProfileSkip::current().checkpoints
            || wal_bytes < target
            || self
                .last_attempt
                .is_some_and(|attempt| attempt.elapsed() < WAL_CHECKPOINT_RETRY_INTERVAL)
        {
            return;
        }
        let _ = self.checkpoint(connection, telemetry, bootstrap_active);
    }

    fn checkpoint(
        &mut self,
        connection: &Connection,
        telemetry: &WriterTelemetry,
        reader_free: bool,
    ) -> Result<CheckpointOutcome, EngineError> {
        self.last_attempt = Some(Instant::now());
        let started = Instant::now();
        let result = if reader_free {
            reader_free_checkpoint(connection)
        } else {
            controlled_checkpoint(connection)
        };
        telemetry.record_checkpoint(&result, started.elapsed());
        result
    }
}

struct WriterTelemetry {
    opened_at: Instant,
    commit_attempts: AtomicU64,
    committed: AtomicU64,
    failed: AtomicU64,
    facts_committed: AtomicU64,
    changes_published: AtomicU64,
    sqlite_rows_changed: AtomicU64,
    queue_high_watermark: AtomicU64,
    checkpoint_attempts: AtomicU64,
    checkpoint_completed: AtomicU64,
    checkpoint_blocked: AtomicU64,
    checkpoint_failures: AtomicU64,
    checkpoint_last_log_frames: AtomicU64,
    checkpoint_last_checkpointed_frames: AtomicU64,
    checkpoint_last_remaining_frames: AtomicU64,
    checkpoint_blocked_since_ns: AtomicU64,
    checkpoint_blocked_total_ns: AtomicU64,
    checkpoint: LatencyHistogram,
    queue_wait: LatencyHistogram,
    disk_reserve: LatencyHistogram,
    prepare: LatencyHistogram,
    canonical_projection: LatencyHistogram,
    runtime_projection: LatencyHistogram,
    usage_projection: LatencyHistogram,
    cursor_and_catalog: LatencyHistogram,
    change_log: LatencyHistogram,
    maintenance: LatencyHistogram,
    sqlite_commit: LatencyHistogram,
    physical_transaction: LatencyHistogram,
    physical_sqlite_commit: LatencyHistogram,
    writer_total: LatencyHistogram,
    bootstrap_finalize: LatencyHistogram,
    history_and_fact_storage: LatencyHistogram,
    session_index: LatencyHistogram,
    project_memory: LatencyHistogram,
    persisted_tool_result: LatencyHistogram,
    interpretation_settings: LatencyHistogram,
    run_state: LatencyHistogram,
    delegation: LatencyHistogram,
    presence: LatencyHistogram,
    team: LatencyHistogram,
    task: LatencyHistogram,
    artifact: LatencyHistogram,
    workflow: LatencyHistogram,
    usage_aggregation: LatencyHistogram,
}

impl WriterTelemetry {
    fn new() -> Self {
        Self {
            opened_at: Instant::now(),
            commit_attempts: AtomicU64::new(0),
            committed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            facts_committed: AtomicU64::new(0),
            changes_published: AtomicU64::new(0),
            sqlite_rows_changed: AtomicU64::new(0),
            queue_high_watermark: AtomicU64::new(0),
            checkpoint_attempts: AtomicU64::new(0),
            checkpoint_completed: AtomicU64::new(0),
            checkpoint_blocked: AtomicU64::new(0),
            checkpoint_failures: AtomicU64::new(0),
            checkpoint_last_log_frames: AtomicU64::new(0),
            checkpoint_last_checkpointed_frames: AtomicU64::new(0),
            checkpoint_last_remaining_frames: AtomicU64::new(0),
            checkpoint_blocked_since_ns: AtomicU64::new(0),
            checkpoint_blocked_total_ns: AtomicU64::new(0),
            checkpoint: LatencyHistogram::default(),
            queue_wait: LatencyHistogram::default(),
            disk_reserve: LatencyHistogram::default(),
            prepare: LatencyHistogram::default(),
            canonical_projection: LatencyHistogram::default(),
            runtime_projection: LatencyHistogram::default(),
            usage_projection: LatencyHistogram::default(),
            cursor_and_catalog: LatencyHistogram::default(),
            change_log: LatencyHistogram::default(),
            maintenance: LatencyHistogram::default(),
            sqlite_commit: LatencyHistogram::default(),
            physical_transaction: LatencyHistogram::default(),
            physical_sqlite_commit: LatencyHistogram::default(),
            writer_total: LatencyHistogram::default(),
            bootstrap_finalize: LatencyHistogram::default(),
            history_and_fact_storage: LatencyHistogram::default(),
            session_index: LatencyHistogram::default(),
            project_memory: LatencyHistogram::default(),
            persisted_tool_result: LatencyHistogram::default(),
            interpretation_settings: LatencyHistogram::default(),
            run_state: LatencyHistogram::default(),
            delegation: LatencyHistogram::default(),
            presence: LatencyHistogram::default(),
            team: LatencyHistogram::default(),
            task: LatencyHistogram::default(),
            artifact: LatencyHistogram::default(),
            workflow: LatencyHistogram::default(),
            usage_aggregation: LatencyHistogram::default(),
        }
    }

    fn observe_queue_depth(&self, depth: usize) {
        atomic_max(
            &self.queue_high_watermark,
            u64::try_from(depth).unwrap_or(u64::MAX),
        );
    }

    fn record_commit_result(
        &self,
        result: &Result<CommitReceipt, EngineError>,
        fact_count: u64,
        rows_before: u64,
        rows_after: u64,
        hook: &CommitTimingHook,
        include_details: bool,
    ) {
        match result {
            Ok(_) => {
                atomic_saturating_add(&self.committed, 1);
                atomic_saturating_add(&self.facts_committed, fact_count);
                atomic_saturating_add(
                    &self.sqlite_rows_changed,
                    rows_after.saturating_sub(rows_before),
                );
                hook.record_into(self, include_details);
            }
            Err(_) => atomic_saturating_add(&self.failed, 1),
        }
    }

    fn record_checkpoint(
        &self,
        result: &Result<CheckpointOutcome, EngineError>,
        elapsed: Duration,
    ) {
        atomic_saturating_add(&self.checkpoint_attempts, 1);
        self.checkpoint.record(elapsed);
        let now_ns = duration_ns(self.opened_at.elapsed()).max(1);
        match result {
            Ok(outcome) => {
                self.checkpoint_last_log_frames
                    .store(outcome.log_frames, Ordering::Release);
                self.checkpoint_last_checkpointed_frames
                    .store(outcome.checkpointed_frames, Ordering::Release);
                self.checkpoint_last_remaining_frames
                    .store(outcome.remaining_frames, Ordering::Release);
                if outcome.busy || outcome.remaining_frames > 0 {
                    atomic_saturating_add(&self.checkpoint_blocked, 1);
                    let _ = self.checkpoint_blocked_since_ns.compare_exchange(
                        0,
                        now_ns,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                } else {
                    atomic_saturating_add(&self.checkpoint_completed, 1);
                    let blocked_since = self.checkpoint_blocked_since_ns.swap(0, Ordering::AcqRel);
                    if blocked_since > 0 {
                        atomic_saturating_add(
                            &self.checkpoint_blocked_total_ns,
                            now_ns.saturating_sub(blocked_since),
                        );
                    }
                }
            }
            Err(_) => atomic_saturating_add(&self.checkpoint_failures, 1),
        }
    }

    fn snapshot(&self, queue_depth: usize) -> WriterPerformanceSnapshot {
        let uptime_ns = duration_ns(self.opened_at.elapsed());
        let blocked_since = self.checkpoint_blocked_since_ns.load(Ordering::Acquire);
        let blocked_by_reader_ns = self
            .checkpoint_blocked_total_ns
            .load(Ordering::Acquire)
            .saturating_add(if blocked_since > 0 {
                uptime_ns.saturating_sub(blocked_since)
            } else {
                0
            });
        WriterPerformanceSnapshot {
            uptime_ns,
            commit_attempts: self.commit_attempts.load(Ordering::Acquire),
            committed: self.committed.load(Ordering::Acquire),
            failed: self.failed.load(Ordering::Acquire),
            facts_committed: self.facts_committed.load(Ordering::Acquire),
            changes_published: self.changes_published.load(Ordering::Acquire),
            sqlite_rows_changed: self.sqlite_rows_changed.load(Ordering::Acquire),
            queue_depth: u64::try_from(queue_depth).unwrap_or(u64::MAX),
            queue_high_watermark: self.queue_high_watermark.load(Ordering::Acquire),
            checkpoint: CheckpointPerformanceSnapshot {
                attempts: self.checkpoint_attempts.load(Ordering::Acquire),
                completed: self.checkpoint_completed.load(Ordering::Acquire),
                blocked: self.checkpoint_blocked.load(Ordering::Acquire),
                failures: self.checkpoint_failures.load(Ordering::Acquire),
                last_log_frames: self.checkpoint_last_log_frames.load(Ordering::Acquire),
                last_checkpointed_frames: self
                    .checkpoint_last_checkpointed_frames
                    .load(Ordering::Acquire),
                last_remaining_frames: self
                    .checkpoint_last_remaining_frames
                    .load(Ordering::Acquire),
                blocked_by_reader_ns,
                latency: self.checkpoint.snapshot(),
            },
            timings: [
                ("queue_wait", &self.queue_wait),
                ("disk_reserve", &self.disk_reserve),
                ("prepare", &self.prepare),
                ("canonical_projection", &self.canonical_projection),
                ("runtime_projection", &self.runtime_projection),
                ("usage_projection", &self.usage_projection),
                ("cursor_and_catalog", &self.cursor_and_catalog),
                ("change_log", &self.change_log),
                ("maintenance", &self.maintenance),
                ("sqlite_commit", &self.sqlite_commit),
                ("physical_transaction", &self.physical_transaction),
                ("physical_sqlite_commit", &self.physical_sqlite_commit),
                ("writer_total", &self.writer_total),
                ("bootstrap_finalize", &self.bootstrap_finalize),
                (
                    "projector.history_and_fact_storage",
                    &self.history_and_fact_storage,
                ),
                ("projector.session_index", &self.session_index),
                ("projector.project_memory", &self.project_memory),
                (
                    "projector.persisted_tool_result",
                    &self.persisted_tool_result,
                ),
                (
                    "projector.interpretation_settings",
                    &self.interpretation_settings,
                ),
                ("projector.run_state", &self.run_state),
                ("projector.delegation", &self.delegation),
                ("projector.presence", &self.presence),
                ("projector.team", &self.team),
                ("projector.task", &self.task),
                ("projector.artifact", &self.artifact),
                ("projector.workflow", &self.workflow),
                ("projector.usage_aggregation", &self.usage_aggregation),
            ]
            .into_iter()
            .map(|(name, histogram)| NamedLatencySnapshot {
                name: name.to_string(),
                latency: histogram.snapshot(),
            })
            .collect(),
        }
    }
}

struct CommitTimingHook {
    started_at: Instant,
    marks_ns: [AtomicU64; 8],
    details_ns: [AtomicU64; 13],
}

impl CommitTimingHook {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            marks_ns: std::array::from_fn(|_| AtomicU64::new(0)),
            details_ns: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn record_into(&self, telemetry: &WriterTelemetry, include_details: bool) {
        let marks = self
            .marks_ns
            .each_ref()
            .map(|mark| mark.load(Ordering::Acquire));
        telemetry.prepare.record_ns(marks[0]);
        telemetry
            .canonical_projection
            .record_ns(marks[1].saturating_sub(marks[0]));
        telemetry
            .runtime_projection
            .record_ns(marks[2].saturating_sub(marks[1]));
        telemetry
            .usage_projection
            .record_ns(marks[3].saturating_sub(marks[2]));
        telemetry
            .cursor_and_catalog
            .record_ns(marks[4].saturating_sub(marks[3]));
        telemetry
            .change_log
            .record_ns(marks[5].saturating_sub(marks[4]));
        telemetry
            .maintenance
            .record_ns(marks[6].saturating_sub(marks[5]));
        telemetry
            .sqlite_commit
            .record_ns(marks[7].saturating_sub(marks[6]));
        if !include_details {
            return;
        }
        let details = self
            .details_ns
            .each_ref()
            .map(|detail| detail.load(Ordering::Acquire));
        for (histogram, elapsed_ns) in [
            &telemetry.history_and_fact_storage,
            &telemetry.session_index,
            &telemetry.project_memory,
            &telemetry.persisted_tool_result,
            &telemetry.interpretation_settings,
            &telemetry.run_state,
            &telemetry.delegation,
            &telemetry.presence,
            &telemetry.team,
            &telemetry.task,
            &telemetry.artifact,
            &telemetry.workflow,
            &telemetry.usage_aggregation,
        ]
        .into_iter()
        .zip(details)
        {
            histogram.record_ns(elapsed_ns);
        }
    }

    fn attribute_group_commit(&self, elapsed: Duration) {
        let before_commit = self.marks_ns[6].load(Ordering::Acquire);
        self.marks_ns[7].store(
            before_commit.saturating_add(duration_ns(elapsed)),
            Ordering::Release,
        );
    }
}

impl CommitHook for CommitTimingHook {
    fn reach(&self, stage: CommitStage) -> Result<(), EngineError> {
        let index = match stage {
            CommitStage::BeforeTransaction => 0,
            CommitStage::MidCanonicalProjection => 1,
            CommitStage::MidRuntimeProjection => 2,
            CommitStage::MidUsageProjection => 3,
            CommitStage::AfterCursorUpdate => 4,
            CommitStage::AfterOutboxInsert => 5,
            CommitStage::BeforeCommit => 6,
            CommitStage::AfterCommit => 7,
            // Publication occurs after the writer response on the engine
            // caller, so the transaction hook cannot truthfully time it.
            CommitStage::BeforePublish => return Ok(()),
        };
        self.marks_ns[index].store(duration_ns(self.started_at.elapsed()), Ordering::Release);
        Ok(())
    }

    fn record_detail(&self, detail: CommitDetail, elapsed: Duration) {
        // The hook cannot borrow the writer telemetry during the transaction,
        // so retain only fixed per-detail nanoseconds until successful commit.
        let index = match detail {
            CommitDetail::HistoryAndFactStorage => 0,
            CommitDetail::SessionIndex => 1,
            CommitDetail::ProjectMemory => 2,
            CommitDetail::PersistedToolResult => 3,
            CommitDetail::InterpretationSettings => 4,
            CommitDetail::RunState => 5,
            CommitDetail::Delegation => 6,
            CommitDetail::Presence => 7,
            CommitDetail::Team => 8,
            CommitDetail::Task => 9,
            CommitDetail::Artifact => 10,
            CommitDetail::Workflow => 11,
            CommitDetail::UsageAggregation => 12,
        };
        self.details_ns[index].store(duration_ns(elapsed), Ordering::Release);
    }
}

fn sqlite_total_changes(connection: &Connection) -> u64 {
    connection
        .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|changes| u64::try_from(changes).ok())
        .unwrap_or_default()
}

fn controlled_checkpoint(connection: &Connection) -> Result<CheckpointOutcome, EngineError> {
    let passive = read_checkpoint_row(
        connection,
        "PRAGMA wal_checkpoint(PASSIVE)",
        "run passive WAL checkpoint",
    )?;
    let passive_remaining = passive
        .log_frames
        .saturating_sub(passive.checkpointed_frames);
    if passive.busy || passive_remaining > 0 {
        return Ok(CheckpointOutcome {
            busy: true,
            log_frames: passive.log_frames,
            checkpointed_frames: passive.checkpointed_frames,
            remaining_frames: passive_remaining,
        });
    }

    // Reclaim the already-checkpointed WAL allocation without ever waiting
    // behind a reader. The writer restores its normal busy timeout before it
    // accepts the next command.
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|error| EngineError::Sqlite {
            operation: "disable busy wait for WAL truncation",
            detail: error.to_string(),
        })?;
    let truncate = read_checkpoint_row(
        connection,
        "PRAGMA wal_checkpoint(TRUNCATE)",
        "truncate checkpointed WAL",
    );
    let restore = connection
        .busy_timeout(WRITER_BUSY_TIMEOUT)
        .map_err(|error| EngineError::Sqlite {
            operation: "restore writer busy timeout after WAL truncation",
            detail: error.to_string(),
        });
    let truncate = truncate?;
    restore?;

    Ok(CheckpointOutcome {
        busy: truncate.busy,
        log_frames: passive.log_frames,
        checkpointed_frames: passive.checkpointed_frames,
        // A busy TRUNCATE means the physical WAL generation is still pinned
        // even when PASSIVE copied every frame into the main database.
        remaining_frames: if truncate.busy { passive.log_frames } else { 0 },
    })
}

/// Bootstrap has no query readers by construction, so one TRUNCATE operation
/// can copy and reclaim the WAL. The live controller must retain its two-step
/// nonblocking PASSIVE/TRUNCATE protocol because a pinned reader is normal
/// there; applying that protocol to a reader-free multi-gigabyte build merely
/// pays a second synchronization boundary for every checkpoint.
fn reader_free_checkpoint(connection: &Connection) -> Result<CheckpointOutcome, EngineError> {
    let checkpoint = read_checkpoint_row(
        connection,
        "PRAGMA wal_checkpoint(TRUNCATE)",
        "run reader-free WAL checkpoint",
    )?;
    let remaining_frames = if checkpoint.busy {
        let uncopied = checkpoint
            .log_frames
            .saturating_sub(checkpoint.checkpointed_frames);
        if uncopied == 0 {
            checkpoint.log_frames
        } else {
            uncopied
        }
    } else {
        0
    };
    Ok(CheckpointOutcome {
        busy: checkpoint.busy,
        log_frames: checkpoint.log_frames,
        checkpointed_frames: checkpoint.checkpointed_frames,
        remaining_frames,
    })
}

struct CheckpointRow {
    busy: bool,
    log_frames: u64,
    checkpointed_frames: u64,
}

fn read_checkpoint_row(
    connection: &Connection,
    sql: &'static str,
    operation: &'static str,
) -> Result<CheckpointRow, EngineError> {
    let (busy, log_frames, checkpointed_frames) = connection
        .query_row(sql, [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| EngineError::Sqlite {
            operation,
            detail: error.to_string(),
        })?;
    Ok(CheckpointRow {
        busy: busy != 0,
        log_frames: u64::try_from(log_frames).unwrap_or_default(),
        checkpointed_frames: u64::try_from(checkpointed_frames).unwrap_or_default(),
    })
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
    let mut connection = Connection::open(database_path).map_err(|error| EngineError::Sqlite {
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
        .busy_timeout(WRITER_BUSY_TIMEOUT)
        .map_err(|error| EngineError::Sqlite {
            operation: "configure writer busy timeout",
            detail: error.to_string(),
        })?;
    schema::set_pragmas(&connection).map_err(|error| EngineError::Sqlite {
        operation: "configure writer pragmas",
        detail: error.to_string(),
    })?;
    apply_ingest_profile_pragmas(&connection)?;
    schema::initialize_schema(&connection).map_err(|error| EngineError::Sqlite {
        operation: "initialize schema",
        detail: error.to_string(),
    })?;
    if query_bootstrap_active(&connection)? {
        let checkpoint = reader_free_checkpoint(&connection)?;
        require_reader_free_checkpoint(checkpoint)?;
        finalize_query_bootstrap_connection(&mut connection)?;
        let checkpoint = reader_free_checkpoint(&connection)?;
        require_reader_free_checkpoint(checkpoint)?;
    }
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

fn query_bootstrap_active(connection: &Connection) -> Result<bool, EngineError> {
    schema::query_bootstrap_state(connection)
        .map(|state| state.is_some())
        .map_err(|error| EngineError::Sqlite {
            operation: "read durable query bootstrap state",
            detail: error.to_string(),
        })
}

fn finalize_query_bootstrap_connection(
    connection: &mut Connection,
) -> Result<Option<u64>, EngineError> {
    if super::ingest_profile::IngestProfileSkip::current().finalize {
        return clear_query_bootstrap_marker(connection);
    }
    // Stay on WAL + NORMAL. Crash recovery already re-runs finalization, so
    // switching a multi-gigabyte file to DELETE + FULL only adds fsyncs.
    schema::set_bootstrap_ingest_pragmas(connection).map_err(|error| EngineError::Sqlite {
        operation: "retain bootstrap cache for index finalization",
        detail: error.to_string(),
    })?;

    let skip = super::ingest_profile::IngestProfileSkip::current();
    let finalization = if skip.relaxes_sqlite_constraints() {
        schema::finalize_query_bootstrap_skip_fk_check(connection)
    } else {
        schema::finalize_query_bootstrap(connection)
    }
    .map_err(|error| EngineError::Sqlite {
        operation: "finalize durable query bootstrap",
        detail: error.to_string(),
    });
    let restore = schema::set_pragmas(connection)
        .map_err(|error| EngineError::Sqlite {
            operation: "restore writer pragmas after query bootstrap",
            detail: error.to_string(),
        })
        .and_then(|()| apply_ingest_profile_pragmas(connection));
    match (finalization, restore) {
        (Ok(watermark), Ok(())) => Ok(watermark),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn clear_query_bootstrap_marker(connection: &Connection) -> Result<Option<u64>, EngineError> {
    if !query_bootstrap_active(connection)? {
        return Ok(None);
    }
    let watermark: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(commit_seq), 0) FROM ingest_commits WHERE committed_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|error| EngineError::Sqlite {
            operation: "read skipped bootstrap watermark",
            detail: error.to_string(),
        })?;
    connection
        .execute(
            "DELETE FROM schema_meta WHERE key = 'query_bootstrap_state'",
            [],
        )
        .map_err(|error| EngineError::Sqlite {
            operation: "clear skipped query bootstrap marker",
            detail: error.to_string(),
        })?;
    Ok(Some(u64::try_from(watermark).unwrap_or_default()))
}

fn apply_ingest_profile_pragmas(connection: &Connection) -> Result<(), EngineError> {
    if !super::ingest_profile::IngestProfileSkip::current().relaxes_sqlite_constraints() {
        return Ok(());
    }
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .map_err(|error| EngineError::Sqlite {
            operation: "relax foreign keys for ingest profile skip",
            detail: error.to_string(),
        })
}

fn require_reader_free_checkpoint(outcome: CheckpointOutcome) -> Result<(), EngineError> {
    if outcome.busy || outcome.remaining_frames != 0 {
        return Err(EngineError::Sqlite {
            operation: "prepare reader-free query bootstrap finalization",
            detail: format!(
                "checkpoint remained busy={} with {} WAL frames",
                outcome.busy, outcome.remaining_frames
            ),
        });
    }
    Ok(())
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
    use crate::adapter::RawRetentionPolicy;
    use tempfile::tempdir;

    fn grouped_request(index: u8) -> ObservationCommit {
        ObservationCommit {
            source: commit::SourceInstanceSpec {
                adapter_id: "writer-group-fixture".to_string(),
                stable_key: vec![index],
                display_name: format!("fixture-{index}"),
                adapter_version: "1.0.0".to_string(),
                adapter_contract_version: 1,
                source_schema_versions: Vec::new(),
                capabilities: Vec::new(),
                discovered_at: 1,
                last_seen_at: 1,
            },
            stream: commit::SourceStreamSpec {
                stream_key: "records".to_string(),
                driver_kind: "replace_document".to_string(),
                decoder_key: "fixture".to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: Some(1),
                consistency: crate::adapter::ConsistencyPolicy::SnapshotReplace,
                retention: RawRetentionPolicy::HashOnly,
            },
            object: commit::SourceObjectUpdate {
                object_key: vec![index],
                expected: commit::ExpectedSourceCursor::Absent,
                display_path: None,
                native_identity: None,
                generation: 1,
                committed_cursor: b"complete".to_vec(),
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
            reason: "group-test".to_string(),
            started_at: 1,
            committed_at: 2,
            fact_count: 0,
            projection_versions: Vec::new(),
            record_errors: Vec::new(),
            changes: Vec::new(),
        }
    }

    fn commit_command(
        request: ObservationCommit,
    ) -> (WriterCommand, Receiver<Result<CommitReceipt, EngineError>>) {
        let (response, receive) = bounded(1);
        (
            WriterCommand::Commit {
                request: Box::new(request),
                queued_at: Instant::now(),
                response,
            },
            receive,
        )
    }

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

    #[test]
    fn bootstrap_collects_queued_commits_up_to_the_fact_bound() {
        let (tx, rx) = bounded(8);
        let (first, first_rx) = commit_command(grouped_request(1));
        let mut held = vec![first_rx];
        for index in 2..=5 {
            let (command, response) = commit_command(grouped_request(index));
            tx.send(command).unwrap();
            held.push(response);
        }
        let (group, leftover) = collect_commit_group(&rx, first, true);
        assert_eq!(group.len(), 5);
        assert!(leftover.is_none());
        drop(tx);
        drop(held);
    }

    #[test]
    fn queued_logical_commits_share_one_physical_transaction() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("grouped-commit.db");
        let mut connection = open_writer(&database).unwrap();
        let telemetry = WriterTelemetry::new();
        let mut checkpoints = CheckpointController::new(&database);
        let mut commands = Vec::new();
        let mut responses = Vec::new();
        for index in 1..=4 {
            let (command, response) = commit_command(grouped_request(index));
            commands.push(command);
            responses.push(response);
        }

        process_commit_group(
            &mut connection,
            &database,
            &telemetry,
            &mut checkpoints,
            commands,
            true,
            false,
        );

        for response in responses {
            response.recv().unwrap().unwrap();
        }
        let snapshot = telemetry.snapshot(0);
        assert_eq!(snapshot.commit_attempts, 4);
        assert_eq!(snapshot.committed, 4);
        assert_eq!(snapshot.failed, 0);
        assert_eq!(
            snapshot
                .timings
                .iter()
                .find(|timing| timing.name == "physical_transaction")
                .unwrap()
                .latency
                .samples,
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            4
        );
    }

    #[test]
    fn failed_group_rolls_back_before_isolated_retry() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("grouped-fallback.db");
        let mut connection = open_writer(&database).unwrap();
        let telemetry = WriterTelemetry::new();
        let mut checkpoints = CheckpointController::new(&database);
        let duplicate = grouped_request(1);
        let (first, first_response) = commit_command(duplicate.clone());
        let (second, second_response) = commit_command(duplicate);

        process_commit_group(
            &mut connection,
            &database,
            &telemetry,
            &mut checkpoints,
            vec![first, second],
            true,
            false,
        );

        first_response.recv().unwrap().unwrap();
        assert!(matches!(
            second_response.recv().unwrap(),
            Err(EngineError::StaleSourceCursor { .. })
        ));
        let snapshot = telemetry.snapshot(0);
        assert_eq!(snapshot.commit_attempts, 2);
        assert_eq!(snapshot.committed, 1);
        assert_eq!(snapshot.failed, 1);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn bootstrap_commits_omit_public_change_log_rows() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("bootstrap-changelog.db");
        let mut connection = open_writer(&database).unwrap();
        let telemetry = WriterTelemetry::new();
        let mut checkpoints = CheckpointController::new(&database);
        let mut request = grouped_request(1);
        request.changes.push(commit::ChangeEntry {
            topic: "history.message.changed".to_string(),
            schema_version: 1,
            entity_key: b"message".to_vec(),
            operation: "upsert".to_string(),
            payload: Vec::new(),
        });
        let (command, response) = commit_command(request);

        process_commit_group(
            &mut connection,
            &database,
            &telemetry,
            &mut checkpoints,
            vec![command],
            true,
            true,
        );
        response.recv().unwrap().unwrap();

        let change_log_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM change_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            change_log_rows, 0,
            "bootstrap ingest publishes a snapshot watermark instead of historical change-log rows"
        );
        let commits: i64 = connection
            .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(commits, 1);
    }

    #[test]
    fn bootstrap_defers_checkpoints_until_the_large_wal_threshold() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("bootstrap-checkpoint.db");
        let connection = open_writer(&database).unwrap();
        let telemetry = WriterTelemetry::new();
        let mut checkpoints = CheckpointController::new(&database);
        checkpoints.maybe_checkpoint(&connection, &telemetry, true);
        assert_eq!(telemetry.snapshot(0).checkpoint.attempts, 0);
        checkpoints.maybe_checkpoint(&connection, &telemetry, false);
        assert_eq!(
            telemetry.snapshot(0).checkpoint.attempts,
            0,
            "an empty WAL stays below both live and bootstrap thresholds"
        );
    }

    #[test]
    fn reader_free_bootstrap_checkpoint_copies_and_reclaims_the_wal() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("reader-free-checkpoint.db");
        let connection = open_writer(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE checkpoint_fixture(value BLOB NOT NULL); \
                 BEGIN IMMEDIATE; \
                 INSERT INTO checkpoint_fixture(value) VALUES (zeroblob(1048576)); \
                 COMMIT;",
            )
            .unwrap();

        let mut wal_path = database.as_os_str().to_os_string();
        wal_path.push("-wal");
        let wal_path = PathBuf::from(wal_path);
        assert!(
            std::fs::metadata(&wal_path)
                .map(|metadata| metadata.len())
                .unwrap_or_default()
                > 0,
            "fixture must create WAL frames before the checkpoint"
        );

        let checkpoint = reader_free_checkpoint(&connection).unwrap();
        assert!(!checkpoint.busy, "{checkpoint:?}");
        assert_eq!(checkpoint.remaining_frames, 0);
        assert_eq!(
            std::fs::metadata(wal_path)
                .map(|metadata| metadata.len())
                .unwrap_or_default(),
            0,
            "reader-free TRUNCATE must reclaim the WAL"
        );
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
