//! Native watch-before-scan supervisor for the common observation coordinator.
//!
//! Filesystem callbacks enqueue invalidation only. One bounded worker owns the
//! watcher, coalescing window, polling backstop, and coordinator dispatch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{after, bounded, select, Receiver, Sender};
use notify::{event::ModifyKind, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::adapter::{
    AgentAdapter, DiscoveryContext, DriverSpec, SourceInstance, SourceInstanceSpec, StreamAuthority,
};
use crate::source::{confined_relative_path_key, DirtyReason, IngestPriority, PollingPolicy};

use super::coordinator::SelectorPatterns;
use super::observation::PendingObservationWork;
use super::{
    EngineError, ObservationCoordinator, QueryCancellationToken, ReconcileRequest,
    ReconcileRetryTarget, SpaghettiEngineCore,
};

const WATCH_EVENT_CAPACITY: usize = 1_024;
const CONTROL_CAPACITY: usize = 16;
const COALESCE_WINDOW: Duration = Duration::from_millis(20);
// Native events remain the primary source of truth. This slow audit only
// catches silent backend loss; watcher-less supervisors retain the adaptive
// 500 ms / 5 s polling policy below.
const WATCHER_AUDIT_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MAX_RECONCILE_PASSES_PER_WAKE: usize = 16;
const MAX_STARTUP_SETTLE_WINDOWS: usize = 16;
const MAX_REASON_BYTES: usize = 128;

#[derive(Debug, Clone)]
pub struct ObservationSupervisorOptions {
    pub configured_roots: Vec<PathBuf>,
    pub reason: String,
}

impl ObservationSupervisorOptions {
    pub fn new(configured_roots: Vec<PathBuf>) -> Self {
        Self {
            configured_roots,
            reason: "native_watch".to_string(),
        }
    }
}

enum SupervisorCommand {
    Refresh {
        cancellation: QueryCancellationToken,
        response: Sender<Result<(), EngineError>>,
    },
    PauseForBootstrap {
        paused: Sender<Result<(), EngineError>>,
        resume: Receiver<()>,
        completed: Sender<Result<(), EngineError>>,
    },
    Shutdown,
}

enum SupervisorStartup {
    BeginInitialScan {
        cancellation: QueryCancellationToken,
    },
    Shutdown,
}

enum WatchIngress {
    Event(Event),
    BackendError,
}

type WatcherFactory = fn(
    Weak<SpaghettiEngineCore>,
    String,
    Arc<WatchTopology>,
    Sender<()>,
) -> Result<RecommendedWatcher, EngineError>;

#[derive(Debug)]
struct WatchedInstance {
    stable_key: Vec<u8>,
    spec: SourceInstanceSpec,
    event_roots: Vec<PathBuf>,
    routes: Vec<WatchRoute>,
}

#[derive(Debug)]
struct WatchRoute {
    roots: Vec<PathBuf>,
    stream_key: String,
    directory_snapshot: bool,
    patterns: SelectorPatterns,
}

#[derive(Debug, Default)]
struct WatchTopology {
    instances: Vec<WatchedInstance>,
    physical_roots: Vec<PathBuf>,
}

#[derive(Debug)]
struct StartReady {
    topology_instances: usize,
    physical_roots: usize,
}

pub(crate) struct ObservationSupervisor {
    adapter_id: String,
    watched_instances: u32,
    watch_roots: u32,
    commands: Sender<SupervisorCommand>,
    alive: Arc<AtomicBool>,
    watcher_available: Arc<AtomicBool>,
    cancellation: QueryCancellationToken,
    join: Option<JoinHandle<()>>,
}

pub(crate) struct PreparedObservationSupervisor {
    inner: Option<PreparedObservationSupervisorInner>,
}

pub(crate) struct StartingObservationSupervisor {
    inner: Option<PreparedObservationSupervisorInner>,
}

struct PreparedObservationSupervisorInner {
    adapter_id: String,
    watched_instances: u32,
    watch_roots: u32,
    commands: Sender<SupervisorCommand>,
    startup: Sender<SupervisorStartup>,
    started: Receiver<Result<(), EngineError>>,
    alive: Arc<AtomicBool>,
    watcher_available: Arc<AtomicBool>,
    cancellation: QueryCancellationToken,
    startup_cancellation: QueryCancellationToken,
    join: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct ObservationSupervisorClient {
    commands: Sender<SupervisorCommand>,
    alive: Arc<AtomicBool>,
}

pub(crate) struct PausedObservationSupervisor {
    resume: Option<Sender<()>>,
    completed: Receiver<Result<(), EngineError>>,
}

impl ObservationSupervisor {
    pub(crate) fn prepare_cancellable<A: AgentAdapter>(
        engine: Arc<SpaghettiEngineCore>,
        adapter: A,
        options: ObservationSupervisorOptions,
        startup_cancellation: QueryCancellationToken,
    ) -> Result<PreparedObservationSupervisor, EngineError> {
        Self::prepare_with_watcher_factory(
            engine,
            adapter,
            options,
            create_registered_watcher,
            startup_cancellation,
        )
    }

    fn prepare_with_watcher_factory<A: AgentAdapter>(
        engine: Arc<SpaghettiEngineCore>,
        adapter: A,
        options: ObservationSupervisorOptions,
        watcher_factory: WatcherFactory,
        startup_cancellation: QueryCancellationToken,
    ) -> Result<PreparedObservationSupervisor, EngineError> {
        validate_options(&options)?;
        let options = normalize_options(options)?;
        adapter
            .manifest()
            .validate()
            .map_err(|error| supervisor_error("validate adapter manifest", error))?;
        let adapter_id = adapter.manifest().id.as_str().to_string();
        let (command_tx, command_rx) = bounded(CONTROL_CAPACITY);
        let (startup_tx, startup_rx) = bounded(1);
        let (prepared_tx, prepared_rx) = bounded(1);
        let (started_tx, started_rx) = bounded(1);
        let alive = Arc::new(AtomicBool::new(false));
        let thread_alive = Arc::clone(&alive);
        let watcher_available = Arc::new(AtomicBool::new(false));
        let thread_watcher_available = Arc::clone(&watcher_available);
        let cancellation = QueryCancellationToken::default();
        let thread_cancellation = cancellation.clone();
        let thread_startup_cancellation = startup_cancellation.clone();
        let worker_adapter_id = adapter_id.clone();
        let weak_engine = Arc::downgrade(&engine);

        let join = thread::Builder::new()
            .name(format!("spaghetti-watch-{adapter_id}"))
            .spawn(move || {
                supervisor_thread(
                    adapter,
                    SupervisorThreadContext {
                        engine: weak_engine,
                        options,
                        commands: command_rx,
                        startup: startup_rx,
                        prepared: prepared_tx,
                        started: started_tx,
                        alive: thread_alive,
                        watcher_available: thread_watcher_available,
                        watcher_factory,
                        cancellation: thread_cancellation,
                        startup_cancellation: thread_startup_cancellation,
                    },
                );
            })
            .map_err(|error| EngineError::WorkerStart {
                worker: "observation supervisor",
                detail: error.to_string(),
            })?;

        match prepared_rx.recv() {
            Ok(Ok(ready)) => {
                debug_assert!(ready.topology_instances > 0);
                Ok(PreparedObservationSupervisor {
                    inner: Some(PreparedObservationSupervisorInner {
                        adapter_id: worker_adapter_id,
                        watched_instances: bounded_u32(ready.topology_instances),
                        watch_roots: bounded_u32(ready.physical_roots),
                        commands: command_tx,
                        startup: startup_tx,
                        started: started_rx,
                        alive,
                        watcher_available,
                        cancellation,
                        startup_cancellation,
                        join: Some(join),
                    }),
                })
            }
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(EngineError::WorkerStart {
                    worker: "observation supervisor",
                    detail: "supervisor exited before reporting watcher preparation".to_string(),
                })
            }
        }
    }

    pub(crate) fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub(crate) fn watched_instances(&self) -> u32 {
        self.watched_instances
    }

    pub(crate) fn watch_roots(&self) -> u32 {
        if self.watcher_available.load(Ordering::Acquire) {
            self.watch_roots
        } else {
            0
        }
    }

    pub(crate) fn watcher_available(&self) -> bool {
        self.watcher_available.load(Ordering::Acquire)
    }

    pub(crate) fn client(&self) -> ObservationSupervisorClient {
        ObservationSupervisorClient {
            commands: self.commands.clone(),
            alive: Arc::clone(&self.alive),
        }
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), EngineError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        self.cancellation.cancel();
        let _ = self.commands.send(SupervisorCommand::Shutdown);
        join.join().map_err(|_| EngineError::WorkerPanic {
            worker: "observation supervisor",
        })
    }
}

impl PreparedObservationSupervisor {
    pub(crate) fn start(self) -> Result<ObservationSupervisor, EngineError> {
        self.begin()?.finish()
    }

    pub(crate) fn begin(self) -> Result<StartingObservationSupervisor, EngineError> {
        let cancellation = self.inner().startup_cancellation.clone();
        self.begin_with_cancellation(cancellation)
    }

    pub(crate) fn begin_with_cancellation(
        mut self,
        cancellation: QueryCancellationToken,
    ) -> Result<StartingObservationSupervisor, EngineError> {
        let mut inner = self
            .inner
            .take()
            .expect("prepared supervisor retains its worker until start or drop");
        if inner.startup_cancellation.is_cancelled() {
            inner.shutdown();
            return Err(EngineError::QueryCancelled);
        }
        if inner
            .startup
            .send(SupervisorStartup::BeginInitialScan { cancellation })
            .is_err()
        {
            inner.shutdown();
            return Err(EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            });
        }
        Ok(StartingObservationSupervisor { inner: Some(inner) })
    }

    fn inner(&self) -> &PreparedObservationSupervisorInner {
        self.inner
            .as_ref()
            .expect("prepared supervisor retains its worker until start or drop")
    }
}

impl StartingObservationSupervisor {
    pub(crate) fn finish(mut self) -> Result<ObservationSupervisor, EngineError> {
        let mut inner = self
            .inner
            .take()
            .expect("starting supervisor retains its worker until finish or drop");
        match inner.started.recv() {
            Ok(Ok(())) => Ok(ObservationSupervisor {
                adapter_id: inner.adapter_id,
                watched_instances: inner.watched_instances,
                watch_roots: inner.watch_roots,
                commands: inner.commands,
                alive: inner.alive,
                watcher_available: inner.watcher_available,
                cancellation: inner.cancellation,
                join: inner.join.take(),
            }),
            Ok(Err(error)) => {
                inner.shutdown();
                Err(error)
            }
            Err(_) => {
                inner.shutdown();
                Err(EngineError::WorkerStart {
                    worker: "observation supervisor",
                    detail: "supervisor exited before completing its initial scan".to_string(),
                })
            }
        }
    }
}

impl PreparedObservationSupervisorInner {
    fn shutdown(&mut self) {
        self.cancellation.cancel();
        let _ = self.startup.send(SupervisorStartup::Shutdown);
        let _ = self.commands.send(SupervisorCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for PreparedObservationSupervisor {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            inner.shutdown();
        }
    }
}

impl Drop for StartingObservationSupervisor {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            inner.shutdown();
        }
    }
}

impl ObservationSupervisorClient {
    pub(crate) fn refresh_cancellable(
        &self,
        cancellation: QueryCancellationToken,
    ) -> Result<(), EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(SupervisorCommand::Refresh {
                cancellation,
                response: response_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })?
    }

    pub(crate) fn pause_for_bootstrap(&self) -> Result<PausedObservationSupervisor, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            });
        }
        let (paused_tx, paused_rx) = bounded(1);
        let (resume_tx, resume_rx) = bounded(1);
        let (completed_tx, completed_rx) = bounded(1);
        self.commands
            .send(SupervisorCommand::PauseForBootstrap {
                paused: paused_tx,
                resume: resume_rx,
                completed: completed_tx,
            })
            .map_err(|_| EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })?;
        paused_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })??;
        Ok(PausedObservationSupervisor {
            resume: Some(resume_tx),
            completed: completed_rx,
        })
    }
}

impl PausedObservationSupervisor {
    pub(crate) fn resume(mut self) -> Result<(), EngineError> {
        self.signal_resume()?;
        self.completed
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })?
    }

    fn signal_resume(&mut self) -> Result<(), EngineError> {
        let Some(resume) = self.resume.take() else {
            return Ok(());
        };
        resume.send(()).map_err(|_| EngineError::WorkerUnavailable {
            worker: "observation supervisor",
        })
    }
}

impl Drop for PausedObservationSupervisor {
    fn drop(&mut self) {
        let _ = self.signal_resume();
    }
}

impl Drop for ObservationSupervisor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct SupervisorThreadContext {
    engine: Weak<SpaghettiEngineCore>,
    options: ObservationSupervisorOptions,
    commands: Receiver<SupervisorCommand>,
    startup: Receiver<SupervisorStartup>,
    prepared: Sender<Result<StartReady, EngineError>>,
    started: Sender<Result<(), EngineError>>,
    alive: Arc<AtomicBool>,
    watcher_available: Arc<AtomicBool>,
    watcher_factory: WatcherFactory,
    cancellation: QueryCancellationToken,
    startup_cancellation: QueryCancellationToken,
}

fn supervisor_thread<A: AgentAdapter>(adapter: A, context: SupervisorThreadContext) {
    let SupervisorThreadContext {
        engine,
        options,
        commands,
        startup,
        prepared,
        started,
        alive,
        watcher_available,
        watcher_factory,
        cancellation,
        startup_cancellation,
    } = context;
    let adapter_id = adapter.manifest().id.as_str().to_string();
    let topology = match discover_topology(&adapter, &options.configured_roots) {
        Ok(topology) => topology,
        Err(error) => {
            let _ = prepared.send(Err(error));
            return;
        }
    };
    let topology = Arc::new(topology);
    if startup_cancellation.is_cancelled() {
        let _ = prepared.send(Err(EngineError::QueryCancelled));
        return;
    }
    let (wake_tx, wake_rx) = bounded(WATCH_EVENT_CAPACITY);
    let mut watcher = match watcher_factory(
        engine.clone(),
        adapter_id.clone(),
        Arc::clone(&topology),
        wake_tx.clone(),
    ) {
        Ok(watcher) => {
            watcher_available.store(true, Ordering::Release);
            Some(watcher)
        }
        Err(_) => None,
    };

    if prepared
        .send(Ok(StartReady {
            topology_instances: topology.instances.len(),
            physical_roots: topology.physical_roots.len(),
        }))
        .is_err()
    {
        return;
    }
    let scan_cancellation = match startup.recv() {
        Ok(SupervisorStartup::BeginInitialScan { cancellation }) => cancellation,
        Ok(SupervisorStartup::Shutdown) | Err(_) => return,
    };
    if scan_cancellation.is_cancelled() || cancellation.is_cancelled() {
        let _ = started.send(Err(EngineError::QueryCancelled));
        return;
    }

    // Watch registration is complete before the first scan begins. Callbacks
    // admit dirty state synchronously; their bounded channel only wakes this
    // worker, so an event during the scan cannot be lost or acknowledged by
    // the lease that began before it.
    let Some(initial_engine) = engine.upgrade() else {
        let _ = started.send(Err(EngineError::ShuttingDown));
        return;
    };
    let startup_cancellations = [cancellation.clone(), scan_cancellation.clone()];
    loop {
        let result = {
            let _source_pass = match initial_engine.acquire_source_pass(IngestPriority::Backfill) {
                Ok(source_pass) => source_pass,
                Err(error) => {
                    let _ = started.send(Err(error));
                    return;
                }
            };
            ObservationCoordinator::with_cancellations(
                Arc::clone(&initial_engine),
                startup_cancellations.to_vec(),
            )
            .reconcile(
                &adapter,
                ReconcileRequest {
                    configured_roots: options.configured_roots.clone(),
                    reason: format!("{}_initial_scan", options.reason),
                },
            )
        };
        match result {
            Ok(_) => break,
            Err(EngineError::ObservationBusy) => {
                if let Err(error) =
                    initial_engine.wait_for_observation_idle_cancellable(&startup_cancellations)
                {
                    let _ = started.send(Err(error));
                    return;
                }
            }
            Err(error) => {
                let _ = started.send(Err(error));
                return;
            }
        }
    }
    let mut polling = PollingPolicy::default();
    if watcher.is_none() {
        polling.record_watcher_unavailable();
        // With no watcher callback to cover the initial scan window, retain a
        // second full pass before readiness. Future polls remain authoritative
        // until registration succeeds.
        let _ =
            initial_engine.require_observation_reconcile(&adapter_id, DirtyReason::BackendError);
    }
    let initial_summary = drain_until_caught_up(
        &initial_engine,
        &adapter,
        &options,
        &topology,
        &mut watcher,
        &watcher_available,
        &mut polling,
        &[cancellation.clone(), scan_cancellation.clone()],
    );
    if let Err(error) = initial_summary.result {
        let _ = started.send(Err(error));
        return;
    }
    let mut startup_settled = false;
    for _ in 0..MAX_STARTUP_SETTLE_WINDOWS {
        match wake_rx.recv_timeout(COALESCE_WINDOW) {
            Ok(()) => {
                while wake_rx.try_recv().is_ok() {}
                let summary = drain_until_caught_up(
                    &initial_engine,
                    &adapter,
                    &options,
                    &topology,
                    &mut watcher,
                    &watcher_available,
                    &mut polling,
                    &[cancellation.clone(), scan_cancellation.clone()],
                );
                if let Err(error) = summary.result {
                    let _ = started.send(Err(error));
                    return;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                startup_settled = true;
                break;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                startup_settled = true;
                break;
            }
        }
    }
    if !startup_settled {
        let _ = started.send(Err(EngineError::Observation {
            operation: "settle startup watcher",
            detail: "native changes did not quiesce within the bounded startup window".to_string(),
        }));
        return;
    }
    drop(initial_engine);

    alive.store(true, Ordering::Release);
    if started.send(Ok(())).is_err() {
        alive.store(false, Ordering::Release);
        return;
    }

    let mut poll = after(next_poll_delay(&polling, watcher.is_some()));
    loop {
        select! {
            recv(commands) -> command => match command {
                Ok(SupervisorCommand::Refresh { cancellation: refresh_cancellation, response }) => {
                    let result = engine.upgrade().ok_or(EngineError::ShuttingDown).and_then(|engine| {
                        engine
                            .require_observation_reconcile(&adapter_id, DirtyReason::ManualRepair)
                            .and_then(|()| {
                                let summary = drain_pending(
                                    &engine,
                                    &adapter,
                                    &options,
                                    &topology,
                                    MAX_RECONCILE_PASSES_PER_WAKE,
                                    &[cancellation.clone(), refresh_cancellation],
                                );
                                update_polling_after_drain(&mut polling, &summary);
                                handle_backend_failure(
                                    &summary,
                                    &mut watcher,
                                    &watcher_available,
                                    &mut polling,
                                );
                                if summary.immediate_retry {
                                    let _ = wake_tx.try_send(());
                                }
                                summary.result
                            })
                    });
                    let _ = response.send(result);
                }
                Ok(SupervisorCommand::PauseForBootstrap { paused, resume, completed }) => {
                    let Some(engine) = engine.upgrade() else {
                        let _ = paused.send(Err(EngineError::ShuttingDown));
                        break;
                    };
                    let before = drain_until_caught_up(
                        &engine,
                        &adapter,
                        &options,
                        &topology,
                        &mut watcher,
                        &watcher_available,
                        &mut polling,
                        std::slice::from_ref(&cancellation),
                    );
                    if let Err(error) = before.result {
                        let _ = paused.send(Err(error));
                        continue;
                    }
                    if paused.send(Ok(())).is_err() {
                        continue;
                    }
                    // The watcher remains registered and admits dirty state
                    // while the sole writer rebuilds query structures. The
                    // worker itself is quiescent until the engine resumes it.
                    let _ = resume.recv();
                    let after = drain_until_caught_up(
                        &engine,
                        &adapter,
                        &options,
                        &topology,
                        &mut watcher,
                        &watcher_available,
                        &mut polling,
                        std::slice::from_ref(&cancellation),
                    );
                    let _ = completed.send(after.result);
                }
                Ok(SupervisorCommand::Shutdown) | Err(_) => break,
            },
            recv(wake_rx) -> wake => {
                if wake.is_ok() {
                    polling.record_activity(now_monotonic_ms());
                    let deadline = Instant::now() + COALESCE_WINDOW;
                    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                        match wake_rx.recv_timeout(remaining) {
                            Ok(()) => {}
                            Err(_) => break,
                        }
                    }
                    let Some(engine) = engine.upgrade() else { break; };
                    let summary = drain_pending(
                        &engine,
                        &adapter,
                        &options,
                        &topology,
                        MAX_RECONCILE_PASSES_PER_WAKE,
                        std::slice::from_ref(&cancellation),
                    );
                    update_polling_after_drain(&mut polling, &summary);
                    handle_backend_failure(
                        &summary,
                        &mut watcher,
                        &watcher_available,
                        &mut polling,
                    );
                    if summary.immediate_retry {
                        let _ = wake_tx.try_send(());
                    }
                }
            },
            recv(poll) -> _ => {
                let Some(engine) = engine.upgrade() else { break; };
                if watcher.is_none() {
                    match watcher_factory(
                        Arc::downgrade(&engine),
                        adapter_id.clone(),
                        Arc::clone(&topology),
                        wake_tx.clone(),
                    ) {
                        Ok(recovered) => {
                            watcher = Some(recovered);
                            watcher_available.store(true, Ordering::Release);
                            polling.record_watcher_success();
                        }
                        Err(_) => polling.record_watcher_unavailable(),
                    }
                }
                if engine.next_observation_work(&adapter_id).is_none() {
                    let _ = engine.require_observation_reconcile(
                        &adapter_id,
                        DirtyReason::PollDetectedChange,
                    );
                }
                let summary = drain_pending(
                    &engine,
                    &adapter,
                    &options,
                    &topology,
                    MAX_RECONCILE_PASSES_PER_WAKE,
                    std::slice::from_ref(&cancellation),
                );
                update_polling_after_drain(&mut polling, &summary);
                handle_backend_failure(
                    &summary,
                    &mut watcher,
                    &watcher_available,
                    &mut polling,
                );
                if summary.immediate_retry {
                    let _ = wake_tx.try_send(());
                }
            },
        }
        poll = after(next_poll_delay(&polling, watcher.is_some()));
    }
    drop(watcher);
    watcher_available.store(false, Ordering::Release);
    alive.store(false, Ordering::Release);
}

fn handle_backend_failure(
    summary: &DrainSummary,
    watcher: &mut Option<RecommendedWatcher>,
    watcher_available: &AtomicBool,
    polling: &mut PollingPolicy,
) {
    if summary.backend_failure {
        *watcher = None;
        watcher_available.store(false, Ordering::Release);
        polling.record_watcher_unavailable();
    }
}

#[allow(clippy::too_many_arguments)]
fn drain_until_caught_up<A: AgentAdapter>(
    engine: &Arc<SpaghettiEngineCore>,
    adapter: &A,
    options: &ObservationSupervisorOptions,
    topology: &WatchTopology,
    watcher: &mut Option<RecommendedWatcher>,
    watcher_available: &AtomicBool,
    polling: &mut PollingPolicy,
    cancellations: &[QueryCancellationToken],
) -> DrainSummary {
    loop {
        let summary = drain_pending(
            engine,
            adapter,
            options,
            topology,
            MAX_RECONCILE_PASSES_PER_WAKE,
            cancellations,
        );
        update_polling_after_drain(polling, &summary);
        handle_backend_failure(&summary, watcher, watcher_available, polling);
        if summary.result.is_err() || !summary.immediate_retry {
            return summary;
        }
        // Known finite cursor backlog remains bounded per coordinator pass,
        // but bootstrap readiness waits for every such pass to converge.
        thread::yield_now();
    }
}

fn discover_topology<A: AgentAdapter>(
    adapter: &A,
    configured_roots: &[PathBuf],
) -> Result<WatchTopology, EngineError> {
    let observed_at = now_unix_ms()?;
    let specs = adapter
        .discover(&DiscoveryContext {
            configured_roots: configured_roots.to_vec(),
            observed_at,
        })
        .map_err(|error| supervisor_error("discover watch topology", error))?;
    if specs.is_empty() {
        return Err(EngineError::InvalidConfig(
            "observation supervisor discovered no source instances".to_string(),
        ));
    }

    let mut instances = Vec::with_capacity(specs.len());
    let mut physical_roots = Vec::new();
    for spec in specs {
        // Stream declarations are topology-only here: the adapter cannot use
        // this placeholder ID for source reads, decoding, or persistence.
        let routing_instance = SourceInstance {
            id: 0,
            spec: spec.clone(),
        };
        let streams = adapter
            .streams(&routing_instance)
            .map_err(|error| supervisor_error("declare watch routes", error))?;
        let mut routes = Vec::with_capacity(streams.len());
        for stream in streams {
            stream
                .validate(&routing_instance)
                .map_err(|error| supervisor_error("validate watch route", error))?;
            if stream.authority == StreamAuthority::IgnoredDerived {
                continue;
            }
            let root = routing_instance
                .root(&stream.selector.root_name)
                .map_err(|error| supervisor_error("resolve watch route root", error))?
                .to_path_buf();
            routes.push(WatchRoute {
                roots: event_path_aliases(&root, configured_roots),
                stream_key: stream.id.as_str().to_string(),
                directory_snapshot: matches!(stream.driver, DriverSpec::DirectorySnapshot(_)),
                patterns: SelectorPatterns::new(&stream)?,
            });
        }
        let mut roots = Vec::new();
        let mut event_roots = Vec::new();
        for root in &spec.roots {
            event_roots.extend(event_path_aliases(&root.path, configured_roots));
            if let Some(existing) = nearest_existing_ancestor(&root.path) {
                roots.push(existing);
            }
        }
        consolidate_roots(&mut roots);
        consolidate_roots(&mut event_roots);
        if roots.is_empty() {
            return Err(EngineError::InvalidConfig(format!(
                "source instance {} has no watchable roots",
                spec.display_name
            )));
        }
        physical_roots.extend(roots.iter().cloned());
        instances.push(WatchedInstance {
            stable_key: spec.stable_key.as_bytes().to_vec(),
            spec,
            event_roots,
            routes,
        });
    }
    consolidate_roots(&mut physical_roots);
    Ok(WatchTopology {
        instances,
        physical_roots,
    })
}

fn create_watcher(
    engine: Weak<SpaghettiEngineCore>,
    adapter_id: String,
    topology: Arc<WatchTopology>,
    wake: Sender<()>,
) -> Result<RecommendedWatcher, EngineError> {
    notify::recommended_watcher(move |event| {
        let ingress = match event {
            Ok(event) => WatchIngress::Event(event),
            Err(_) => WatchIngress::BackendError,
        };
        if let Some(engine) = engine.upgrade() {
            route_ingress(&engine, &adapter_id, &topology, ingress);
            // Dirty admission above is already bounded and loss-aware. This
            // queue is only a coalesced wake signal, so Full is safe.
            let _ = wake.try_send(());
        }
    })
    .map_err(|error| EngineError::WorkerStart {
        worker: "native filesystem watcher",
        detail: error.to_string(),
    })
}

fn create_registered_watcher(
    engine: Weak<SpaghettiEngineCore>,
    adapter_id: String,
    topology: Arc<WatchTopology>,
    wake: Sender<()>,
) -> Result<RecommendedWatcher, EngineError> {
    let mut watcher = create_watcher(engine, adapter_id, Arc::clone(&topology), wake)?;
    register_roots(&mut watcher, &topology.physical_roots)?;
    Ok(watcher)
}

fn register_roots(watcher: &mut RecommendedWatcher, roots: &[PathBuf]) -> Result<(), EngineError> {
    for root in roots {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| EngineError::WorkerStart {
                worker: "native filesystem watcher",
                detail: error.to_string(),
            })?;
    }
    Ok(())
}

fn route_ingress(
    engine: &SpaghettiEngineCore,
    adapter_id: &str,
    topology: &WatchTopology,
    ingress: WatchIngress,
) {
    match ingress {
        WatchIngress::BackendError => {
            let _ = engine.require_observation_reconcile(adapter_id, DirtyReason::BackendError);
        }
        WatchIngress::Event(event) if event.need_rescan() => {
            let _ = engine.require_observation_reconcile(adapter_id, DirtyReason::WatcherOverflow);
        }
        WatchIngress::Event(event) if should_ignore_event(&event.kind) => {}
        WatchIngress::Event(event) if event.paths.is_empty() => {
            let _ = engine.require_observation_reconcile(adapter_id, DirtyReason::NativeEvent);
        }
        WatchIngress::Event(event) => {
            let membership_change = is_membership_event(&event.kind);
            for instance in &topology.instances {
                let mut instance_dirty = false;
                let mut object_routes = BTreeSet::new();
                for path in event
                    .paths
                    .iter()
                    .filter(|path| instance_contains(instance, path))
                {
                    for route in &instance.routes {
                        let relative_path = route
                            .roots
                            .iter()
                            .find_map(|root| path.strip_prefix(root).ok());
                        let Some(relative_path) = relative_path else {
                            // A membership event for a newly-created logical
                            // root can arrive on an ancestor watched in its
                            // place. Instance discovery is required to see it.
                            if membership_change
                                && route.roots.iter().any(|root| root.starts_with(path))
                            {
                                instance_dirty = true;
                            }
                            continue;
                        };
                        if !route.patterns.matches(relative_path) {
                            continue;
                        }
                        // File-set changes need catalog discovery. Directory
                        // snapshots can also carry cross-file context, so any
                        // selected change conservatively reconciles the one
                        // affected instance.
                        if membership_change || route.directory_snapshot {
                            instance_dirty = true;
                            break;
                        }
                        match confined_relative_path_key(relative_path) {
                            Ok(object_key) => {
                                object_routes.insert((route.stream_key.clone(), object_key));
                            }
                            Err(_) => {
                                instance_dirty = true;
                                break;
                            }
                        }
                    }
                    if instance_dirty {
                        break;
                    }
                }
                if instance_dirty {
                    let _ = engine.mark_observation_instance_dirty(
                        adapter_id,
                        &instance.stable_key,
                        DirtyReason::NativeEvent,
                    );
                    continue;
                }
                for (stream_key, object_key) in object_routes {
                    let _ = engine.mark_observation_object_dirty(
                        adapter_id,
                        &instance.stable_key,
                        &stream_key,
                        &object_key,
                        DirtyReason::NativeEvent,
                    );
                }
            }
        }
    }
}

#[derive(Debug)]
struct DrainSummary {
    result: Result<(), EngineError>,
    retries_required: bool,
    immediate_retry: bool,
    incomplete_tail_retry: bool,
    changed: bool,
    watcher_failure: bool,
    backend_failure: bool,
    watcher_success: bool,
}

impl Default for DrainSummary {
    fn default() -> Self {
        Self {
            result: Ok(()),
            retries_required: false,
            immediate_retry: false,
            incomplete_tail_retry: false,
            changed: false,
            watcher_failure: false,
            backend_failure: false,
            watcher_success: false,
        }
    }
}

struct DrainedObservation {
    outcome: super::ReconcileOutcome,
    reason: DirtyReason,
}

fn drain_pending<A: AgentAdapter>(
    engine: &Arc<SpaghettiEngineCore>,
    adapter: &A,
    options: &ObservationSupervisorOptions,
    topology: &WatchTopology,
    max_passes: usize,
    cancellations: &[QueryCancellationToken],
) -> DrainSummary {
    let mut summary = DrainSummary::default();
    let mut completed_passes = 0;
    while completed_passes < max_passes {
        if cancellations
            .iter()
            .any(QueryCancellationToken::is_cancelled)
        {
            summary.result = Err(EngineError::QueryCancelled);
            return summary;
        }
        match drain_pending_once(engine, adapter, options, topology, cancellations) {
            Ok(Some(drained)) => {
                completed_passes += 1;
                summary.changed |= drained.outcome.objects_registered > 0
                    || drained.outcome.objects_changed > 0
                    || drained.outcome.objects_removed > 0
                    || drained.outcome.commits > 0;
                summary.watcher_failure |= matches!(
                    drained.reason,
                    DirtyReason::WatcherOverflow
                        | DirtyReason::InternalQueueOverflow
                        | DirtyReason::BackendError
                );
                summary.backend_failure |= drained.reason == DirtyReason::BackendError;
                summary.watcher_success |= drained.reason == DirtyReason::NativeEvent;
                if drained.outcome.retries_required > 0 {
                    summary.retries_required = true;
                    summary.incomplete_tail_retry = drained.outcome.incomplete_tail_retries > 0;
                    if drained.outcome.backlog_remaining == 0 {
                        return summary;
                    }
                    summary.immediate_retry = true;
                }
            }
            Ok(None) => {
                if engine.has_pending_observation_work(adapter.manifest().id.as_str()) {
                    if let Err(error) = engine.wait_for_observation_idle_cancellable(cancellations)
                    {
                        summary.result = Err(error);
                        return summary;
                    }
                    continue;
                }
                summary.immediate_retry = false;
                return summary;
            }
            Err(EngineError::ObservationBusy) => {
                if let Err(error) = engine.wait_for_observation_idle_cancellable(cancellations) {
                    summary.result = Err(error);
                    return summary;
                }
                continue;
            }
            Err(error) => {
                summary.result = Err(error);
                return summary;
            }
        }
        if !engine.has_pending_observation_work(adapter.manifest().id.as_str()) {
            summary.immediate_retry = false;
            return summary;
        }
    }
    summary.immediate_retry = engine.has_pending_observation_work(adapter.manifest().id.as_str());
    summary
}

fn drain_pending_once<A: AgentAdapter>(
    engine: &Arc<SpaghettiEngineCore>,
    adapter: &A,
    options: &ObservationSupervisorOptions,
    topology: &WatchTopology,
    cancellations: &[QueryCancellationToken],
) -> Result<Option<DrainedObservation>, EngineError> {
    if cancellations
        .iter()
        .any(QueryCancellationToken::is_cancelled)
    {
        return Err(EngineError::QueryCancelled);
    }
    let adapter_id = adapter.manifest().id.as_str();
    let Some(work) = engine.next_observation_work(adapter_id) else {
        return Ok(None);
    };
    let reason = match &work {
        PendingObservationWork::Adapter { reason, .. }
        | PendingObservationWork::Instance { reason, .. }
        | PendingObservationWork::Object { reason, .. } => *reason,
    };
    let _source_pass = engine.acquire_source_pass(priority_for_dirty_reason(reason))?;
    match work {
        PendingObservationWork::Adapter {
            adapter_id: pending_adapter,
            reason,
        } => {
            debug_assert_eq!(pending_adapter, adapter_id);
            let outcome = ObservationCoordinator::with_cancellations(
                Arc::clone(engine),
                cancellations.to_vec(),
            )
            .reconcile(
                adapter,
                ReconcileRequest {
                    configured_roots: options.configured_roots.clone(),
                    reason: reason_label(&options.reason, reason),
                },
            )?;
            Ok(Some(DrainedObservation { outcome, reason }))
        }
        PendingObservationWork::Instance {
            adapter_id: pending_adapter,
            stable_key,
            reason,
        } => {
            debug_assert_eq!(pending_adapter, adapter_id);
            let Some(instance) = topology
                .instances
                .iter()
                .find(|instance| instance.stable_key == stable_key)
            else {
                engine.require_observation_reconcile(adapter_id, DirtyReason::IdentityChanged)?;
                return Ok(Some(DrainedObservation {
                    outcome: super::ReconcileOutcome::default(),
                    reason,
                }));
            };
            let outcome = ObservationCoordinator::with_cancellations(
                Arc::clone(engine),
                cancellations.to_vec(),
            )
            .reconcile_declared_instance(
                adapter,
                instance.spec.clone(),
                reason_label(&options.reason, reason),
            )?;
            Ok(Some(DrainedObservation { outcome, reason }))
        }
        PendingObservationWork::Object {
            adapter_id: pending_adapter,
            stable_key,
            stream_key,
            object_key,
            reason,
        } => {
            debug_assert_eq!(pending_adapter, adapter_id);
            let Some(instance) = topology
                .instances
                .iter()
                .find(|instance| instance.stable_key == stable_key)
            else {
                engine.require_observation_reconcile(adapter_id, DirtyReason::IdentityChanged)?;
                return Ok(Some(DrainedObservation {
                    outcome: super::ReconcileOutcome::default(),
                    reason,
                }));
            };
            let target = ReconcileRetryTarget {
                stable_key,
                stream_key,
                object_key,
            };
            let outcome = ObservationCoordinator::with_cancellations(
                Arc::clone(engine),
                cancellations.to_vec(),
            )
            .reconcile_declared_object(
                adapter,
                instance.spec.clone(),
                &target,
                reason_label(&options.reason, reason),
            )?;
            Ok(Some(DrainedObservation { outcome, reason }))
        }
    }
}

fn update_polling_after_drain(policy: &mut PollingPolicy, summary: &DrainSummary) {
    policy.set_incomplete_tail(summary.incomplete_tail_retry);
    if summary.changed {
        policy.record_activity(now_monotonic_ms());
    }
    if summary.result.is_err() || summary.watcher_failure {
        policy.record_watcher_failure();
    } else if summary.watcher_success {
        policy.record_watcher_success();
    }
}

fn next_poll_delay(policy: &PollingPolicy, watcher_available: bool) -> Duration {
    let policy_delay =
        Duration::from_millis(policy.next_delay_ms(now_monotonic_ms().saturating_add(1)));
    if watcher_available && !policy.has_incomplete_tail() {
        policy_delay.max(WATCHER_AUDIT_INTERVAL)
    } else {
        policy_delay
    }
}

fn now_monotonic_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed().as_millis();
    u64::try_from(elapsed).unwrap_or(u64::MAX)
}

fn reason_label(prefix: &str, reason: DirtyReason) -> String {
    format!("{prefix}_{}", dirty_reason_name(reason))
}

fn dirty_reason_name(reason: DirtyReason) -> &'static str {
    match reason {
        DirtyReason::NativeEvent => "native_event",
        DirtyReason::PollDetectedChange => "poll_detected_change",
        DirtyReason::WatcherOverflow => "watcher_overflow",
        DirtyReason::InternalQueueOverflow => "internal_queue_overflow",
        DirtyReason::BackendError => "backend_error",
        DirtyReason::CursorInvalid => "cursor_invalid",
        DirtyReason::IdentityChanged => "identity_changed",
        DirtyReason::RootMoved => "root_moved",
        DirtyReason::Recovery => "recovery",
        DirtyReason::ManualRepair => "manual_repair",
    }
}

fn priority_for_dirty_reason(reason: DirtyReason) -> IngestPriority {
    match reason {
        DirtyReason::NativeEvent | DirtyReason::PollDetectedChange => IngestPriority::Interactive,
        DirtyReason::Recovery => IngestPriority::Backfill,
        DirtyReason::WatcherOverflow
        | DirtyReason::InternalQueueOverflow
        | DirtyReason::BackendError
        | DirtyReason::CursorInvalid
        | DirtyReason::IdentityChanged
        | DirtyReason::RootMoved
        | DirtyReason::ManualRepair => IngestPriority::ForegroundRepair,
    }
}

fn should_ignore_event(kind: &EventKind) -> bool {
    kind.is_access()
}

fn is_membership_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn instance_contains(instance: &WatchedInstance, path: &Path) -> bool {
    instance
        .event_roots
        .iter()
        .any(|root| path.starts_with(root))
}

fn event_path_aliases(path: &Path, configured_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut aliases = vec![path.to_path_buf()];
    for configured_root in configured_roots {
        let Ok(canonical_root) = configured_root.canonicalize() else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(&canonical_root) else {
            continue;
        };
        aliases.push(configured_root.join(relative));
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut candidate = path;
    loop {
        if candidate.is_dir() {
            return candidate.canonicalize().ok();
        }
        candidate = candidate.parent()?;
    }
}

fn consolidate_roots(roots: &mut Vec<PathBuf>) {
    roots.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    let mut consolidated: Vec<PathBuf> = Vec::with_capacity(roots.len());
    for root in roots.drain(..) {
        if consolidated
            .iter()
            .any(|existing| root.starts_with(existing))
        {
            continue;
        }
        consolidated.push(root);
    }
    *roots = consolidated;
}

fn validate_options(options: &ObservationSupervisorOptions) -> Result<(), EngineError> {
    if options.configured_roots.is_empty() || options.reason.trim().is_empty() {
        return Err(EngineError::InvalidConfig(
            "observation supervisor requires at least one configured root and a reason".to_string(),
        ));
    }
    if options.reason.len() > MAX_REASON_BYTES {
        return Err(EngineError::InvalidConfig(
            "observation supervisor reason exceeds 128 bytes".to_string(),
        ));
    }
    Ok(())
}

fn normalize_options(
    mut options: ObservationSupervisorOptions,
) -> Result<ObservationSupervisorOptions, EngineError> {
    let mut unique = BTreeSet::new();
    for root in options.configured_roots {
        let canonical = root
            .canonicalize()
            .map_err(|error| EngineError::Observation {
                operation: "normalize supervisor roots",
                detail: format!("{}: {error}", root.to_string_lossy()),
            })?;
        unique.insert(canonical);
    }
    options.configured_roots = unique.into_iter().collect();
    validate_options(&options)?;
    Ok(options)
}

fn bounded_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn now_unix_ms() -> Result<i64, EngineError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| EngineError::Observation {
            operation: "read supervisor time",
            detail: error.to_string(),
        })?;
    i64::try_from(duration.as_millis()).map_err(|_| EngineError::Observation {
        operation: "read supervisor time",
        detail: "epoch milliseconds overflowed".to_string(),
    })
}

fn supervisor_error(operation: &'static str, error: impl std::fmt::Display) -> EngineError {
    EngineError::Observation {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests;
