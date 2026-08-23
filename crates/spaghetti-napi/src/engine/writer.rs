//! Long-lived engine writer connection.
//!
//! Keeping the `rusqlite::Connection` inside its dedicated thread makes
//! single-writer ownership structural and keeps N-API objects `Send` without
//! moving SQLite handles across runtimes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TryRecvError};
use rusqlite::{Connection, TransactionBehavior};

use crate::adapter::FactBatch;
use crate::core::schema;

use super::catalog::{commit_source_scan, CatalogScanReceipt, SourceScan};
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
    CommitProjectionVersions {
        request: Box<commit::ProjectionVersionCommit>,
        queued_at: Instant,
        response: Sender<Result<Option<commit::ProjectionVersionReceipt>, EngineError>>,
    },
    SourceCatalog {
        adapter_id: String,
        stable_key: Vec<u8>,
        response: Sender<Result<SourceCatalogSnapshot, EngineError>>,
    },
    CommitCatalogScan {
        scan: Box<SourceScan>,
        now_ms: i64,
        response: Sender<Result<CatalogScanReceipt, EngineError>>,
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

    pub(crate) fn commit_projection_versions(
        &self,
        request: commit::ProjectionVersionCommit,
    ) -> Result<Option<commit::ProjectionVersionReceipt>, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::CommitProjectionVersions {
                request: Box::new(request),
                queued_at: Instant::now(),
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

    /// Commit one catalog discovery pass. The writer owns the connection, so
    /// the whole pass lands in one transaction on the single-writer thread.
    pub(crate) fn commit_catalog_scan(
        &self,
        scan: SourceScan,
        now_ms: i64,
    ) -> Result<CatalogScanReceipt, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable { worker: "writer" });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(WriterCommand::CommitCatalogScan {
                scan: Box::new(scan),
                now_ms,
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
            WriterCommand::CommitProjectionVersions {
                request,
                queued_at,
                response,
            } => {
                telemetry.queue_wait.record(queued_at.elapsed());
                let reserve_started = Instant::now();
                let reserve = ensure_disk_reserve(&database_path);
                telemetry.disk_reserve.record(reserve_started.elapsed());
                let rows_before = sqlite_total_changes(&connection);
                let started = Instant::now();
                let result = reserve.and_then(|()| {
                    commit::apply_projection_version_commit(&mut connection, &request)
                });
                let elapsed = started.elapsed();
                telemetry.writer_total.record(elapsed);
                let committed = matches!(result, Ok(Some(_)));
                match &result {
                    Ok(Some(_)) => {
                        atomic_saturating_add(&telemetry.commit_attempts, 1);
                        atomic_saturating_add(&telemetry.committed, 1);
                        atomic_saturating_add(
                            &telemetry.sqlite_rows_changed,
                            sqlite_total_changes(&connection).saturating_sub(rows_before),
                        );
                        telemetry.physical_transaction.record(elapsed);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        atomic_saturating_add(&telemetry.commit_attempts, 1);
                        atomic_saturating_add(&telemetry.failed, 1);
                    }
                }
                let _ = response.send(result);
                if committed {
                    checkpoints.maybe_checkpoint(&connection, &telemetry, bootstrap_active);
                }
            }
            WriterCommand::CommitCatalogScan {
                scan,
                now_ms,
                response,
            } => {
                // A catalog scan is a writer transaction like any other, so it
                // is counted like one: its `ingest_commits` row must be
                // reflected in the writer's committed total.
                let reserve = ensure_disk_reserve(&database_path);
                let rows_before = sqlite_total_changes(&connection);
                let started = Instant::now();
                let result =
                    reserve.and_then(|()| commit_source_scan(&mut connection, &scan, now_ms));
                let elapsed = started.elapsed();
                telemetry.writer_total.record(elapsed);
                atomic_saturating_add(&telemetry.commit_attempts, 1);
                match &result {
                    Ok(_) => {
                        atomic_saturating_add(&telemetry.committed, 1);
                        atomic_saturating_add(
                            &telemetry.sqlite_rows_changed,
                            sqlite_total_changes(&connection).saturating_sub(rows_before),
                        );
                        telemetry.physical_transaction.record(elapsed);
                    }
                    Err(_) => atomic_saturating_add(&telemetry.failed, 1),
                }
                let committed = result.is_ok();
                let _ = response.send(result);
                if committed {
                    checkpoints.maybe_checkpoint(&connection, &telemetry, bootstrap_active);
                }
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
                        let started = Instant::now();
                        let checkpoint = checkpoints.checkpoint(&connection, &telemetry, true);
                        telemetry.record_bootstrap_phase(
                            "bootstrap.pre_finalize_checkpoint",
                            started.elapsed(),
                        );
                        checkpoint?;
                    }
                    let watermark =
                        finalize_query_bootstrap_connection_profiled(&mut connection, &telemetry)?;
                    if !skip.checkpoints && !skip.finalize {
                        let started = Instant::now();
                        let checkpoint = checkpoints.checkpoint(&connection, &telemetry, true);
                        telemetry.record_bootstrap_phase(
                            "bootstrap.post_finalize_checkpoint",
                            started.elapsed(),
                        );
                        checkpoint?;
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
                    bootstrap_active,
                )
            }
            WriterCommand::CommitFacts { request, batch, .. } => {
                projection::apply_fact_observation_commit_in_transaction(
                    &transaction,
                    request,
                    batch,
                    hook,
                    persist_public_changes,
                    bootstrap_active,
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
    history_preparation: LatencyHistogram,
    fact_storage: LatencyHistogram,
    canonical_message_storage: LatencyHistogram,
    history_projection_walk: LatencyHistogram,
    content_block_storage: LatencyHistogram,
    delegation_probe: LatencyHistogram,
    delegation_projection: LatencyHistogram,
    delegation_reductions: LatencyHistogram,
    artifact_preparation: LatencyHistogram,
    artifact_assertion_writes: LatencyHistogram,
    artifact_reductions: LatencyHistogram,
    artifact_cleanup: LatencyHistogram,
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
    bootstrap_phases: Mutex<BTreeMap<String, LatencyHistogram>>,
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
            history_preparation: LatencyHistogram::default(),
            fact_storage: LatencyHistogram::default(),
            canonical_message_storage: LatencyHistogram::default(),
            history_projection_walk: LatencyHistogram::default(),
            content_block_storage: LatencyHistogram::default(),
            delegation_probe: LatencyHistogram::default(),
            delegation_projection: LatencyHistogram::default(),
            delegation_reductions: LatencyHistogram::default(),
            artifact_preparation: LatencyHistogram::default(),
            artifact_assertion_writes: LatencyHistogram::default(),
            artifact_reductions: LatencyHistogram::default(),
            artifact_cleanup: LatencyHistogram::default(),
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
            bootstrap_phases: Mutex::new(BTreeMap::new()),
        }
    }

    fn record_bootstrap_phase(&self, name: &str, elapsed: Duration) {
        self.bootstrap_phases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(name.to_string())
            .or_default()
            .record(elapsed);
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
        let mut timings = [
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
            ("projector.history_preparation", &self.history_preparation),
            ("projector.fact_storage", &self.fact_storage),
            (
                "projector.canonical_message_storage",
                &self.canonical_message_storage,
            ),
            (
                "projector.history_projection_walk",
                &self.history_projection_walk,
            ),
            (
                "projector.content_block_storage",
                &self.content_block_storage,
            ),
            ("projector.delegation_probe", &self.delegation_probe),
            (
                "projector.delegation_projection",
                &self.delegation_projection,
            ),
            (
                "projector.delegation_reductions",
                &self.delegation_reductions,
            ),
            ("projector.artifact_preparation", &self.artifact_preparation),
            (
                "projector.artifact_assertion_writes",
                &self.artifact_assertion_writes,
            ),
            ("projector.artifact_reductions", &self.artifact_reductions),
            ("projector.artifact_cleanup", &self.artifact_cleanup),
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
        .collect::<Vec<_>>();
        timings.extend(
            self.bootstrap_phases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .map(|(name, histogram)| NamedLatencySnapshot {
                    name: name.clone(),
                    latency: histogram.snapshot(),
                }),
        );
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
            timings,
        }
    }
}

struct CommitTimingHook {
    started_at: Instant,
    marks_ns: [AtomicU64; 8],
    details_ns: [AtomicU64; 25],
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
            &telemetry.history_preparation,
            &telemetry.fact_storage,
            &telemetry.canonical_message_storage,
            &telemetry.history_projection_walk,
            &telemetry.content_block_storage,
            &telemetry.delegation_probe,
            &telemetry.delegation_projection,
            &telemetry.delegation_reductions,
            &telemetry.artifact_preparation,
            &telemetry.artifact_assertion_writes,
            &telemetry.artifact_reductions,
            &telemetry.artifact_cleanup,
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
            CommitDetail::HistoryPreparation => 1,
            CommitDetail::FactStorage => 2,
            CommitDetail::CanonicalMessageStorage => 3,
            CommitDetail::HistoryProjectionWalk => 4,
            CommitDetail::ContentBlockStorage => 5,
            CommitDetail::DelegationProbe => 6,
            CommitDetail::DelegationProjection => 7,
            CommitDetail::DelegationReductions => 8,
            CommitDetail::ArtifactPreparation => 9,
            CommitDetail::ArtifactAssertionWrites => 10,
            CommitDetail::ArtifactReductions => 11,
            CommitDetail::ArtifactCleanup => 12,
            CommitDetail::SessionIndex => 13,
            CommitDetail::ProjectMemory => 14,
            CommitDetail::PersistedToolResult => 15,
            CommitDetail::InterpretationSettings => 16,
            CommitDetail::RunState => 17,
            CommitDetail::Delegation => 18,
            CommitDetail::Presence => 19,
            CommitDetail::Team => 20,
            CommitDetail::Task => 21,
            CommitDetail::Artifact => 22,
            CommitDetail::Workflow => 23,
            CommitDetail::UsageAggregation => 24,
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
    let (available_bytes, total_bytes) =
        filesystem_space(filesystem_path).map_err(|error| EngineError::Sqlite {
            operation: "inspect observation database free space",
            detail: error.to_string(),
        })?;
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

#[cfg(unix)]
fn filesystem_space(path: &Path) -> Result<(u64, u64), std::io::Error> {
    let stats = rustix::fs::statvfs(path)?;
    let fragment_bytes = stats.f_frsize.max(stats.f_bsize).max(1);
    Ok((
        stats.f_bavail.saturating_mul(fragment_bytes),
        stats.f_blocks.saturating_mul(fragment_bytes),
    ))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn filesystem_space(path: &Path) -> Result<(u64, u64), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut available_bytes = 0_u64;
    let mut total_bytes = 0_u64;
    let mut free_bytes = 0_u64;
    // SAFETY: `wide_path` is NUL-terminated and all three output pointers are
    // valid, uniquely borrowed `u64` values for the duration of the call.
    let succeeded = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut available_bytes,
            &mut total_bytes,
            &mut free_bytes,
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok((available_bytes, total_bytes))
    }
}

#[cfg(not(any(unix, windows)))]
fn filesystem_space(_path: &Path) -> Result<(u64, u64), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "filesystem capacity inspection is unsupported on this platform",
    ))
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
    // This path repairs a durable bootstrap marker found while opening the
    // writer, so the previous process may have exited during file mutation.
    // Retain a full structural scan before recovery can admit readers.
    finalize_query_bootstrap_connection_observed(connection, true, |_, _| {})
}

fn finalize_query_bootstrap_connection_profiled(
    connection: &mut Connection,
    telemetry: &WriterTelemetry,
) -> Result<Option<u64>, EngineError> {
    // The same live writer created every page in this uninterrupted bootstrap.
    // SQLite has already reported all writes, commits, checkpoints, and DDL as
    // successful. Avoid rescanning the complete fresh file here; foreign-key
    // and FTS semantic audits still gate readiness, and recovery retains the
    // structural scan above.
    let check_database_integrity =
        super::ingest_profile::IngestProfileSkip::current().bootstrap_integrity_deferral;
    finalize_query_bootstrap_connection_observed(
        connection,
        check_database_integrity,
        |phase, elapsed| {
            telemetry.record_bootstrap_phase(&format!("bootstrap.{phase}"), elapsed);
        },
    )
}

fn finalize_query_bootstrap_connection_observed<F>(
    connection: &mut Connection,
    check_database_integrity: bool,
    mut observe: F,
) -> Result<Option<u64>, EngineError>
where
    F: FnMut(&str, Duration),
{
    if super::ingest_profile::IngestProfileSkip::current().finalize {
        return clear_query_bootstrap_marker(connection);
    }
    // Stay on WAL + NORMAL. Crash recovery already re-runs finalization, so
    // switching a multi-gigabyte file to DELETE + FULL only adds fsyncs.
    let started = Instant::now();
    schema::set_bootstrap_ingest_pragmas(connection).map_err(|error| EngineError::Sqlite {
        operation: "retain bootstrap cache for index finalization",
        detail: error.to_string(),
    })?;
    observe("configure_pragmas", started.elapsed());

    let skip = super::ingest_profile::IngestProfileSkip::current();
    let started = Instant::now();
    super::artifact_projection::rebuild_artifacts_for_bootstrap(connection)?;
    observe("artifact_rebuild", started.elapsed());
    let finalization = schema::finalize_query_bootstrap_profiled(
        connection,
        !skip.relaxes_sqlite_constraints(),
        check_database_integrity,
        &mut observe,
    )
    .map_err(|error| EngineError::Sqlite {
        operation: "finalize durable query bootstrap",
        detail: error.to_string(),
    });
    let started = Instant::now();
    let restore = schema::set_pragmas(connection)
        .map_err(|error| EngineError::Sqlite {
            operation: "restore writer pragmas after query bootstrap",
            detail: error.to_string(),
        })
        .and_then(|()| apply_ingest_profile_pragmas(connection));
    observe("restore_pragmas", started.elapsed());
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
mod tests;
