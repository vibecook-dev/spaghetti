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
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::adapter::{AgentAdapter, DiscoveryContext, SourceInstanceSpec};
use crate::source::{DirtyReason, PollingPolicy};

use super::observation::PendingObservationWork;
use super::{EngineError, ObservationCoordinator, ReconcileRequest, SpaghettiEngineCore};

const WATCH_EVENT_CAPACITY: usize = 1_024;
const CONTROL_CAPACITY: usize = 16;
const COALESCE_WINDOW: Duration = Duration::from_millis(20);
const MAX_RECONCILE_PASSES_PER_WAKE: usize = 16;
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
    Refresh(Sender<Result<(), EngineError>>),
    Shutdown,
}

enum WatchIngress {
    Event(Event),
    BackendError,
}

#[derive(Debug, Clone)]
struct WatchedInstance {
    stable_key: Vec<u8>,
    spec: SourceInstanceSpec,
    roots: Vec<PathBuf>,
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
    join: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct ObservationSupervisorClient {
    commands: Sender<SupervisorCommand>,
    alive: Arc<AtomicBool>,
}

impl ObservationSupervisor {
    pub(crate) fn start<A: AgentAdapter>(
        engine: Arc<SpaghettiEngineCore>,
        adapter: A,
        options: ObservationSupervisorOptions,
    ) -> Result<Self, EngineError> {
        validate_options(&options)?;
        let options = normalize_options(options)?;
        adapter
            .manifest()
            .validate()
            .map_err(|error| supervisor_error("validate adapter manifest", error))?;
        let adapter_id = adapter.manifest().id.as_str().to_string();
        let (command_tx, command_rx) = bounded(CONTROL_CAPACITY);
        let (ready_tx, ready_rx) = bounded(1);
        let alive = Arc::new(AtomicBool::new(false));
        let thread_alive = Arc::clone(&alive);
        let worker_adapter_id = adapter_id.clone();
        let weak_engine = Arc::downgrade(&engine);

        let join = thread::Builder::new()
            .name(format!("spaghetti-watch-{adapter_id}"))
            .spawn(move || {
                supervisor_thread(
                    weak_engine,
                    adapter,
                    options,
                    command_rx,
                    ready_tx,
                    thread_alive,
                );
            })
            .map_err(|error| EngineError::WorkerStart {
                worker: "observation supervisor",
                detail: error.to_string(),
            })?;

        match ready_rx.recv() {
            Ok(Ok(ready)) => {
                debug_assert!(ready.topology_instances > 0);
                debug_assert!(ready.physical_roots > 0);
                Ok(Self {
                    adapter_id: worker_adapter_id,
                    watched_instances: bounded_u32(ready.topology_instances),
                    watch_roots: bounded_u32(ready.physical_roots),
                    commands: command_tx,
                    alive,
                    join: Some(join),
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
                    detail: "supervisor exited before reporting readiness".to_string(),
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
        self.watch_roots
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
        let _ = self.commands.send(SupervisorCommand::Shutdown);
        join.join().map_err(|_| EngineError::WorkerPanic {
            worker: "observation supervisor",
        })
    }
}

impl ObservationSupervisorClient {
    pub(crate) fn refresh(&self) -> Result<(), EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            });
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(SupervisorCommand::Refresh(response_tx))
            .map_err(|_| EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })?;
        response_rx
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })?
    }
}

impl Drop for ObservationSupervisor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn supervisor_thread<A: AgentAdapter>(
    engine: Weak<SpaghettiEngineCore>,
    adapter: A,
    options: ObservationSupervisorOptions,
    commands: Receiver<SupervisorCommand>,
    ready: Sender<Result<StartReady, EngineError>>,
    alive: Arc<AtomicBool>,
) {
    let adapter_id = adapter.manifest().id.as_str().to_string();
    let topology = match discover_topology(&adapter, &options.configured_roots) {
        Ok(topology) => topology,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let topology = Arc::new(topology);
    let (wake_tx, wake_rx) = bounded(WATCH_EVENT_CAPACITY);
    let mut watcher = match create_watcher(
        engine.clone(),
        adapter_id.clone(),
        Arc::clone(&topology),
        wake_tx,
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if let Err(error) = register_roots(&mut watcher, &topology.physical_roots) {
        let _ = ready.send(Err(error));
        return;
    }

    // Watch registration is complete before the first scan begins. Callbacks
    // admit dirty state synchronously; their bounded channel only wakes this
    // worker, so an event during the scan cannot be lost or acknowledged by
    // the lease that began before it.
    let Some(initial_engine) = engine.upgrade() else {
        let _ = ready.send(Err(EngineError::ShuttingDown));
        return;
    };
    if let Err(error) = ObservationCoordinator::new(Arc::clone(&initial_engine)).reconcile(
        &adapter,
        ReconcileRequest {
            configured_roots: options.configured_roots.clone(),
            reason: format!("{}_initial_scan", options.reason),
        },
    ) {
        let _ = ready.send(Err(error));
        return;
    }
    let mut polling = PollingPolicy::default();
    let initial_summary = drain_pending(
        &initial_engine,
        &adapter,
        &options,
        &topology,
        MAX_RECONCILE_PASSES_PER_WAKE,
    );
    update_polling_after_drain(&mut polling, &initial_summary);
    if let Err(error) = initial_summary.result {
        let _ = ready.send(Err(error));
        return;
    }
    drop(initial_engine);

    alive.store(true, Ordering::Release);
    if ready
        .send(Ok(StartReady {
            topology_instances: topology.instances.len(),
            physical_roots: topology.physical_roots.len(),
        }))
        .is_err()
    {
        alive.store(false, Ordering::Release);
        return;
    }

    let mut poll = after(next_poll_delay(&polling));
    loop {
        select! {
            recv(commands) -> command => match command {
                Ok(SupervisorCommand::Refresh(response)) => {
                    let result = engine.upgrade().ok_or(EngineError::ShuttingDown).and_then(|engine| {
                        engine
                            .require_observation_reconcile(&adapter_id, DirtyReason::ManualRepair)
                            .and_then(|()| {
                                let summary = drain_pending(&engine, &adapter, &options, &topology, MAX_RECONCILE_PASSES_PER_WAKE);
                                update_polling_after_drain(&mut polling, &summary);
                                summary.result
                            })
                    });
                    let _ = response.send(result);
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
                    );
                    update_polling_after_drain(&mut polling, &summary);
                }
            },
            recv(poll) -> _ => {
                let Some(engine) = engine.upgrade() else { break; };
                let _ = engine.require_observation_reconcile(
                    &adapter_id,
                    DirtyReason::PollDetectedChange,
                );
                let summary = drain_pending(
                    &engine,
                    &adapter,
                    &options,
                    &topology,
                    MAX_RECONCILE_PASSES_PER_WAKE,
                );
                update_polling_after_drain(&mut polling, &summary);
            },
        }
        poll = after(next_poll_delay(&polling));
    }
    drop(watcher);
    alive.store(false, Ordering::Release);
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
        let mut roots = Vec::new();
        for root in &spec.roots {
            if let Some(existing) = nearest_existing_ancestor(&root.path) {
                roots.push(existing);
            }
        }
        consolidate_roots(&mut roots);
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
            roots,
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
            let mut matched = false;
            for instance in &topology.instances {
                if event
                    .paths
                    .iter()
                    .any(|path| instance_contains(instance, path))
                {
                    matched = true;
                    let _ = engine.mark_observation_instance_dirty(
                        adapter_id,
                        &instance.stable_key,
                        DirtyReason::NativeEvent,
                    );
                }
            }
            if !matched {
                let _ = engine.require_observation_reconcile(adapter_id, DirtyReason::NativeEvent);
            }
        }
    }
}

#[derive(Debug)]
struct DrainSummary {
    result: Result<(), EngineError>,
    retries_required: bool,
    changed: bool,
    watcher_failure: bool,
    watcher_success: bool,
}

impl Default for DrainSummary {
    fn default() -> Self {
        Self {
            result: Ok(()),
            retries_required: false,
            changed: false,
            watcher_failure: false,
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
) -> DrainSummary {
    let mut summary = DrainSummary::default();
    for _ in 0..max_passes {
        match drain_pending_once(engine, adapter, options, topology) {
            Ok(Some(drained)) => {
                summary.changed |= drained.outcome.objects_changed > 0;
                summary.watcher_failure |= matches!(
                    drained.reason,
                    DirtyReason::WatcherOverflow
                        | DirtyReason::InternalQueueOverflow
                        | DirtyReason::BackendError
                );
                summary.watcher_success |= drained.reason == DirtyReason::NativeEvent;
                if drained.outcome.retries_required > 0 {
                    summary.retries_required = true;
                    return summary;
                }
            }
            Ok(None) => return summary,
            Err(error) => {
                summary.result = Err(error);
                return summary;
            }
        }
        if engine
            .next_observation_work(adapter.manifest().id.as_str())
            .is_none()
        {
            return summary;
        }
    }
    summary
}

fn drain_pending_once<A: AgentAdapter>(
    engine: &Arc<SpaghettiEngineCore>,
    adapter: &A,
    options: &ObservationSupervisorOptions,
    topology: &WatchTopology,
) -> Result<Option<DrainedObservation>, EngineError> {
    let adapter_id = adapter.manifest().id.as_str();
    let Some(work) = engine.next_observation_work(adapter_id) else {
        return Ok(None);
    };
    match work {
        PendingObservationWork::Adapter {
            adapter_id: pending_adapter,
            reason,
        } => {
            debug_assert_eq!(pending_adapter, adapter_id);
            let outcome = ObservationCoordinator::new(Arc::clone(engine)).reconcile(
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
            let outcome = ObservationCoordinator::new(Arc::clone(engine))
                .reconcile_declared_instance(
                    adapter,
                    instance.spec.clone(),
                    reason_label(&options.reason, reason),
                )?;
            Ok(Some(DrainedObservation { outcome, reason }))
        }
    }
}

fn update_polling_after_drain(policy: &mut PollingPolicy, summary: &DrainSummary) {
    policy.set_incomplete_tail(summary.retries_required);
    if summary.changed {
        policy.record_activity(now_monotonic_ms());
    }
    if summary.result.is_err() || summary.watcher_failure {
        policy.record_watcher_failure();
    } else if summary.watcher_success {
        policy.record_watcher_success();
    }
}

fn next_poll_delay(policy: &PollingPolicy) -> Duration {
    Duration::from_millis(policy.next_delay_ms(now_monotonic_ms().saturating_add(1)))
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

fn should_ignore_event(kind: &EventKind) -> bool {
    kind.is_access()
}

fn instance_contains(instance: &WatchedInstance, path: &Path) -> bool {
    instance
        .spec
        .roots
        .iter()
        .any(|root| path.starts_with(&root.path))
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
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Condvar, Mutex};

    use rusqlite::{Connection, OpenFlags};
    use tempfile::TempDir;

    use crate::adapter::{
        AdapterError, AdapterManifest, AdapterObjectContext, DecodeContext, DecodeDisposition,
        DiscoveryContext, FactBatch, SourceInstance, SourceInstanceSpec, SourceObjectDescriptor,
        StreamSpec,
    };
    use crate::claude::ClaudeCodeAdapter;
    use crate::engine::EngineOptions;
    use crate::source::{platform_path_key, SourceCursor, SourceRecord};

    use super::*;

    #[test]
    fn overlapping_and_missing_logical_roots_consolidate_to_one_native_watch() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(root.join("projects/project")).unwrap();
        let topology =
            discover_topology(&ClaudeCodeAdapter::new(), std::slice::from_ref(&root)).unwrap();

        assert_eq!(topology.instances.len(), 1);
        assert_eq!(topology.physical_roots, vec![root.canonicalize().unwrap()]);
        assert_eq!(topology.instances[0].roots, topology.physical_roots);
    }

    #[test]
    fn path_routing_marks_only_the_affected_instance() {
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let topology =
            discover_topology(&ClaudeCodeAdapter::new(), &[first.clone(), second.clone()]).unwrap();
        assert_eq!(topology.instances.len(), 2);
        let first = first.canonicalize().unwrap();
        assert!(topology.instances.iter().any(|instance| instance
            .spec
            .roots
            .iter()
            .any(|root| first.join("settings.json").starts_with(&root.path))));
        let engine = open_engine(temp.path().join("route.db"));
        let event = Event::new(notify::event::EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(first.join("settings.json"));
        route_ingress(
            &engine,
            "claude-code",
            &topology,
            WatchIngress::Event(event),
        );

        let status = engine.status().observation;
        assert_eq!(status.dirty_instances, 1, "{status:?}");
        assert!(!status.full_reconcile_required, "{status:?}");
        engine.shutdown().unwrap();
    }

    #[test]
    fn overflow_and_backend_errors_escalate_to_adapter_recovery() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        let topology = discover_topology(&ClaudeCodeAdapter::new(), &[root]).unwrap();
        let engine = open_engine(temp.path().join("overflow.db"));

        route_ingress(
            &engine,
            "claude-code",
            &topology,
            WatchIngress::Event(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)),
        );
        assert!(engine.status().observation.recovery_required);
        route_ingress(
            &engine,
            "claude-code",
            &topology,
            WatchIngress::BackendError,
        );
        assert!(engine.status().observation.full_reconcile_required);
        engine.shutdown().unwrap();
    }

    #[test]
    fn polling_cadence_uses_incomplete_active_idle_and_failure_backoff() {
        let mut policy = PollingPolicy::default();
        assert!(next_poll_delay(&policy) >= Duration::from_secs(5));

        let retry = DrainSummary {
            retries_required: true,
            ..DrainSummary::default()
        };
        update_polling_after_drain(&mut policy, &retry);
        assert_eq!(next_poll_delay(&policy), Duration::from_millis(50));

        let active = DrainSummary {
            changed: true,
            ..DrainSummary::default()
        };
        update_polling_after_drain(&mut policy, &active);
        assert_eq!(next_poll_delay(&policy), Duration::from_millis(500));

        let failure = DrainSummary {
            watcher_failure: true,
            ..DrainSummary::default()
        };
        update_polling_after_drain(&mut policy, &failure);
        update_polling_after_drain(&mut policy, &failure);
        update_polling_after_drain(&mut policy, &failure);
        assert!(policy.fallback_active());
        assert_eq!(next_poll_delay(&policy), Duration::from_millis(500));
    }

    #[test]
    fn callback_admitted_inside_initial_scan_is_reconciled_before_ready() {
        const WAIT: Duration = Duration::from_secs(15);
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        let settings = root.join("settings.json");
        std::fs::write(&settings, br#"{"model":"claude-sonnet"}"#).unwrap();
        let database = temp.path().join("race.db");
        let engine = open_engine(database.clone());
        let gate = Arc::new(DecodeGate::default());
        let adapter = GatedClaudeAdapter::new(Arc::clone(&gate));
        let starting_engine = Arc::clone(&engine);
        let starting_root = root.clone();
        let start = thread::spawn(move || {
            starting_engine.start_observation_supervisor(
                adapter,
                ObservationSupervisorOptions::new(vec![starting_root]),
            )
        });

        gate.wait_until_blocked(WAIT);
        assert!(engine.status().observation.reconcile_in_flight);
        std::fs::write(&settings, br#"{"model":"claude-opus"}"#).unwrap();
        let topology =
            discover_topology(&ClaudeCodeAdapter::new(), std::slice::from_ref(&root)).unwrap();
        route_ingress(
            &engine,
            SpaghettiEngineCore::CLAUDE_ADAPTER_ID,
            &topology,
            WatchIngress::Event(
                Event::new(notify::event::EventKind::Modify(
                    notify::event::ModifyKind::Any,
                ))
                .add_path(settings),
            ),
        );
        let during_scan = engine.status().observation;
        assert!(
            during_scan.dirty_instances == 1 || during_scan.full_reconcile_required,
            "{during_scan:?}"
        );
        gate.release();

        start.join().unwrap().unwrap();
        let ready = engine.status().observation;
        assert_eq!(ready.state, "live", "{ready:?}");
        assert_eq!(ready.dirty_instances, 0, "{ready:?}");
        assert!(ready.reconciles_total >= 2, "{ready:?}");
        assert_eq!(effective_settings_model(&database), "claude-opus");
        engine.shutdown().unwrap();
    }

    #[test]
    fn native_supervisor_registers_before_scan_and_reconciles_changes() {
        const WAIT: Duration = Duration::from_secs(15);
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("settings.json"), br#"{"model":"claude-sonnet"}"#).unwrap();
        let database = temp.path().join("engine.db");
        let engine = open_engine(database.clone());
        engine
            .start_observation_supervisor(
                ClaudeCodeAdapter::new(),
                ObservationSupervisorOptions::new(vec![root.clone()]),
            )
            .unwrap();

        let started = engine.status().observation;
        assert_eq!(started.state, "live");
        assert_eq!(started.supervisors_running, 1);
        assert_eq!(started.watched_instances, 1);
        assert_eq!(started.watch_roots, 1);
        assert_eq!(count_settings(&database), 1);
        assert!(engine
            .start_observation_supervisor(
                ClaudeCodeAdapter::new(),
                ObservationSupervisorOptions::new(vec![root.clone()]),
            )
            .is_err());
        // Native backends can surface bootstrap hints after registration; wait
        // for the original supervisor to quiesce before mutating the fixture.
        wait_until(
            WAIT,
            || {
                let observation = engine.status().observation;
                !observation.reconcile_in_flight
                    && observation.dirty_instances == 0
                    && !observation.full_reconcile_required
            },
            || format!("{:?}", engine.status().observation),
        );

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(root.join("settings.json"))
            .unwrap();
        file.write_all(br#"{"model":"claude-opus"}"#).unwrap();
        file.flush().unwrap();
        drop(file);
        wait_until(
            WAIT,
            || {
                let observation = engine.status().observation;
                observation.reconciles_total >= 2
                    && !observation.reconcile_in_flight
                    && observation.state == "live"
            },
            || format!("{:?}", engine.status().observation),
        );
        assert_eq!(engine.status().observation.state, "live");
        assert_eq!(count_settings(&database), 1);

        assert!(engine.stop_observation_supervisor("claude-code").unwrap());
        assert!(!engine.stop_observation_supervisor("claude-code").unwrap());
        assert_eq!(engine.status().observation.supervisors_running, 0);

        engine
            .start_observation_supervisor(
                ClaudeCodeAdapter::new(),
                ObservationSupervisorOptions::new(vec![root]),
            )
            .unwrap();
        assert_eq!(engine.status().observation.supervisors_running, 1);
        engine.shutdown().unwrap();
        let stopped = engine.status();
        assert_eq!(stopped.state, "stopped");
        assert_eq!(stopped.observation.supervisors_running, 0);
    }

    #[test]
    fn supervisor_restart_resumes_the_durable_append_cursor() {
        const WAIT: Duration = Duration::from_secs(15);
        const PROJECT: &str = "-Users-fixture-project";
        const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        let project = root.join(format!("projects/{PROJECT}"));
        std::fs::create_dir_all(&project).unwrap();
        let transcript = project.join(format!("{SESSION}.jsonl"));
        std::fs::write(&transcript, transcript_line(SESSION, "m1", "first")).unwrap();
        let database = temp.path().join("restart.db");

        let first = open_engine(database.clone());
        first
            .start_observation_supervisor(
                ClaudeCodeAdapter::new(),
                ObservationSupervisorOptions::new(vec![root.clone()]),
            )
            .unwrap();
        assert_eq!(count_messages(&database), 1);
        let first_instance_id = source_instance_id(&database);
        let first_cursor = transcript_cursor(&first, &root);
        assert_eq!(
            first_cursor.append_offset_value(),
            Some(file_len(&transcript))
        );
        first.shutdown().unwrap();

        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        append
            .write_all(&transcript_line(SESSION, "m2", "second"))
            .unwrap();
        append.flush().unwrap();
        drop(append);

        let restarted = open_engine(database.clone());
        restarted
            .start_observation_supervisor(
                ClaudeCodeAdapter::new(),
                ObservationSupervisorOptions::new(vec![root.clone()]),
            )
            .unwrap();
        wait_until(
            WAIT,
            || count_messages(&database) == 2,
            || format!("{:?}", restarted.status().observation),
        );
        assert_eq!(source_instance_id(&database), first_instance_id);
        let resumed_cursor = transcript_cursor(&restarted, &root);
        assert_eq!(
            resumed_cursor.append_offset_value(),
            Some(file_len(&transcript))
        );
        assert!(resumed_cursor.append_offset_value() > first_cursor.append_offset_value());
        assert_eq!(restarted.status().observation.state, "live");
        restarted.shutdown().unwrap();
    }

    #[derive(Default)]
    struct DecodeGate {
        calls: AtomicUsize,
        state: Mutex<GateState>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct GateState {
        blocked: bool,
        released: bool,
    }

    impl DecodeGate {
        fn enter(&self) {
            if self.calls.fetch_add(1, AtomicOrdering::AcqRel) != 0 {
                return;
            }
            let mut state = self.state.lock().unwrap();
            state.blocked = true;
            self.changed.notify_all();
            while !state.released {
                state = self.changed.wait(state).unwrap();
            }
        }

        fn wait_until_blocked(&self, timeout: Duration) {
            let deadline = Instant::now() + timeout;
            let mut state = self.state.lock().unwrap();
            while !state.blocked {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("initial reconcile did not reach the decode gate");
                let (next, timed_out) = self.changed.wait_timeout(state, remaining).unwrap();
                state = next;
                assert!(!timed_out.timed_out(), "initial reconcile decode timed out");
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.released = true;
            self.changed.notify_all();
        }
    }

    struct GatedClaudeAdapter {
        inner: ClaudeCodeAdapter,
        gate: Arc<DecodeGate>,
    }

    impl GatedClaudeAdapter {
        fn new(gate: Arc<DecodeGate>) -> Self {
            Self {
                inner: ClaudeCodeAdapter::new(),
                gate,
            }
        }
    }

    impl AgentAdapter for GatedClaudeAdapter {
        fn manifest(&self) -> &AdapterManifest {
            self.inner.manifest()
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            self.inner.discover(context)
        }

        fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            self.inner.streams(instance)
        }

        fn bootstrap_object(
            &self,
            instance: &SourceInstance,
            object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            self.inner.bootstrap_object(instance, object)
        }

        fn decode(
            &self,
            context: DecodeContext<'_>,
            record: &SourceRecord,
            output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            self.gate.enter();
            self.inner.decode(context, record, output)
        }
    }

    fn open_engine(database_path: PathBuf) -> Arc<SpaghettiEngineCore> {
        SpaghettiEngineCore::open(EngineOptions {
            database_path,
            query_workers: Some(1),
            owner_label: Some("supervisor-test".to_string()),
        })
        .unwrap()
    }

    fn count_settings(database: &Path) -> i64 {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM canonical_interpretation_settings_documents",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn effective_settings_model(database: &Path) -> String {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let json: Vec<u8> = connection
            .query_row(
                "SELECT effective_settings_json FROM canonical_effective_interpretation_settings",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&json).unwrap()["model"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn transcript_line(session: &str, message_id: &str, body: &str) -> Vec<u8> {
        let mut line = format!(
            r#"{{"type":"assistant","uuid":"{message_id}","timestamp":"2026-08-12T00:00:00Z","sessionId":"{session}","cwd":"/fixture/project","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"request-{message_id}","message":{{"model":"claude-sonnet","id":"api-{message_id}","type":"message","role":"assistant","content":[{{"type":"text","text":"{body}"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
        )
        .into_bytes();
        line.push(b'\n');
        line
    }

    fn count_messages(database: &Path) -> i64 {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row("SELECT COUNT(*) FROM canonical_messages", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn source_instance_id(database: &Path) -> u64 {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                "SELECT source_instance_id FROM source_instances LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| u64::try_from(value).unwrap())
            .unwrap()
    }

    fn transcript_cursor(engine: &SpaghettiEngineCore, root: &Path) -> SourceCursor {
        let stable_key = platform_path_key(&root.canonicalize().unwrap());
        let object = engine
            .source_catalog(SpaghettiEngineCore::CLAUDE_ADAPTER_ID, &stable_key)
            .unwrap()
            .objects
            .into_iter()
            .find(|object| object.stream_key == "session-transcripts")
            .unwrap();
        SourceCursor::from_opaque(object.committed_cursor).unwrap()
    }

    fn file_len(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().len()
    }

    fn wait_until(
        timeout: Duration,
        predicate: impl Fn() -> bool,
        diagnostic: impl Fn() -> String,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if predicate() {
            return;
        }
        panic!(
            "condition did not become true within {timeout:?}: {}",
            diagnostic()
        );
    }
}
