//! Catalog-first startup for the configured durable host.
//!
//! Order matters and is the whole feature:
//!
//! 1. register every configured source and run its bounded catalog discovery
//!    pass, committing one transaction per source;
//! 2. publish the catalog — projects and sessions are now listable, complete
//!    or explicitly degraded;
//! 3. install watchers and let history, usage, artifacts, and full-text search
//!    converge in the background.
//!
//! Step 1 reads a fraction of a percent of the transcript bytes, so the
//! library is visible in seconds rather than after full ingestion. A warm
//! start does the same work against an already-populated database: the last
//! committed rows are served from the SQLite snapshot the moment the engine
//! opens, and the rescan reconciles by size and modification time.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};

use crate::adapter::{AdapterId, CatalogDiscoveryLimits, DiscoveryContext, SourceInstance};

use super::catalog::{scan_source, SourceScan};
use super::coordinator::{source_instance_spec, validate_request};
use super::supervisor::StartingObservationSupervisor;
use super::{
    EngineError, LifecyclePhase, ObservationSupervisorOptions, QueryCancellationToken,
    ReconcileRequest, SpaghettiEngineCore,
};

const MAX_CONFIGURED_OBSERVATION_SOURCES: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct ConfiguredObservationSource {
    adapter_id: String,
    roots: Vec<PathBuf>,
    reason: String,
}

impl ConfiguredObservationSource {
    pub(crate) fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    pub(crate) fn new(
        adapter_id: impl Into<String>,
        roots: Vec<PathBuf>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            roots,
            reason: reason.into(),
        }
    }
}

/// What one catalog-first startup produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredObservationStartupOutcome {
    /// Projects committed across every configured source.
    pub(crate) catalog_projects: u64,
    /// Sessions committed across every configured source.
    pub(crate) catalog_sessions: u64,
    /// Sources whose discovery pass could not read their complete surface.
    pub(crate) degraded_sources: Vec<String>,
    pub(crate) supervisors_started: usize,
    pub(crate) history_background: bool,
}

/// The message a panic carried, when it carried one.
///
/// A panic payload is almost always the formatted `panic!` string; anything
/// else is reported as unknown rather than guessed at.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return message;
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.as_str();
    }
    "unknown panic payload"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfiguredObservationStartupState {
    Starting,
    Installed,
    Failed,
    Stopped,
}

/// Terminal state plus, for a failure, the underlying error's own message.
///
/// The startup thread used to reduce every failure to `Failed` and drop the
/// error, so a caller was told only that the worker was "unavailable" — the
/// rejected commit, the decode contract error, and a genuine spawn failure
/// were indistinguishable. Keeping the message is what makes the first two
/// diagnosable.
#[derive(Debug, Clone)]
struct ConfiguredObservationStartupProgress {
    state: ConfiguredObservationStartupState,
    failure: Option<String>,
}

struct ConfiguredObservationStartupShared {
    progress: Mutex<ConfiguredObservationStartupProgress>,
    finished: Condvar,
}

impl ConfiguredObservationStartupShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            progress: Mutex::new(ConfiguredObservationStartupProgress {
                state: ConfiguredObservationStartupState::Starting,
                failure: None,
            }),
            finished: Condvar::new(),
        })
    }

    fn set(&self, state: ConfiguredObservationStartupState, failure: Option<String>) {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        progress.state = state;
        progress.failure = failure;
        drop(progress);
        self.finished.notify_all();
    }

    fn get(&self) -> ConfiguredObservationStartupState {
        self.progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state
    }

    fn wait(&self) -> ConfiguredObservationStartupProgress {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while progress.state == ConfiguredObservationStartupState::Starting {
            progress = self
                .finished
                .wait(progress)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        progress.clone()
    }
}

pub(super) struct ConfiguredObservationStartupRuntime {
    adapter_ids: BTreeSet<String>,
    cancellation: QueryCancellationToken,
    shared: Arc<ConfiguredObservationStartupShared>,
    join: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConfiguredObservationStartupStatus {
    pub(super) active: bool,
    pub(super) failed: bool,
}

impl ConfiguredObservationStartupRuntime {
    fn start(
        engine: Weak<SpaghettiEngineCore>,
        adapter_ids: BTreeSet<String>,
        starting: Vec<StartingObservationSupervisor>,
        cancellation: QueryCancellationToken,
    ) -> Result<Self, EngineError> {
        let shared = ConfiguredObservationStartupShared::new();
        let thread_shared = Arc::clone(&shared);
        let thread_cancellation = cancellation.clone();
        let join = thread::Builder::new()
            .name("spaghetti-configured-observation-startup".to_string())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    finish_configured_observation_startup(engine, starting, &thread_cancellation)
                }));
                let (state, failure) = match result {
                    Ok(Ok(())) => (ConfiguredObservationStartupState::Installed, None),
                    Ok(Err(EngineError::QueryCancelled | EngineError::ShuttingDown))
                        if thread_cancellation.is_cancelled() =>
                    {
                        (ConfiguredObservationStartupState::Stopped, None)
                    }
                    Ok(Err(error)) => (
                        ConfiguredObservationStartupState::Failed,
                        Some(error.to_string()),
                    ),
                    Err(panic) => (
                        ConfiguredObservationStartupState::Failed,
                        Some(format!(
                            "startup panicked: {}",
                            panic_message(panic.as_ref())
                        )),
                    ),
                };
                thread_shared.set(state, failure);
            })
            .map_err(|error| EngineError::WorkerStart {
                worker: "configured observation startup",
                detail: error.to_string(),
            })?;
        Ok(Self {
            adapter_ids,
            cancellation,
            shared,
            join: Some(join),
        })
    }

    fn status(&self) -> ConfiguredObservationStartupStatus {
        let state = self.shared.get();
        ConfiguredObservationStartupStatus {
            active: state == ConfiguredObservationStartupState::Starting,
            failed: state == ConfiguredObservationStartupState::Failed,
        }
    }

    fn shutdown(&mut self) -> Result<(), EngineError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        self.cancellation.cancel();
        let result = join.join().map_err(|_| EngineError::WorkerPanic {
            worker: "configured observation startup",
        });
        if self.shared.get() == ConfiguredObservationStartupState::Starting {
            self.shared
                .set(ConfiguredObservationStartupState::Stopped, None);
        }
        result
    }
}

impl Drop for ConfiguredObservationStartupRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn finish_configured_observation_startup(
    engine: Weak<SpaghettiEngineCore>,
    starting: Vec<StartingObservationSupervisor>,
    cancellation: &QueryCancellationToken,
) -> Result<(), EngineError> {
    let mut started = Vec::with_capacity(starting.len());
    for supervisor in starting {
        check_cancelled(cancellation)?;
        started.push(supervisor.finish()?);
    }
    check_cancelled(cancellation)?;
    let engine = engine.upgrade().ok_or(EngineError::ShuttingDown)?;
    engine.install_started_observation_supervisors(started)
}

impl SpaghettiEngineCore {
    /// Start the complete configured durable host, catalog first.
    pub(crate) fn start_configured_observation_cancellable(
        self: &Arc<Self>,
        configured: Vec<ConfiguredObservationSource>,
        cancellation: QueryCancellationToken,
    ) -> Result<ConfiguredObservationStartupOutcome, EngineError> {
        self.ensure_configured_observation_startup_available()?;
        let configured = normalize_configured_sources(configured, &cancellation)?;

        // Catalog before history: every source contributes its rows and the
        // library becomes listable before any watcher thread starts a scan.
        let mut catalog_projects = 0_u64;
        let mut catalog_sessions = 0_u64;
        let mut degraded_sources = Vec::new();
        for source in &configured {
            check_cancelled(&cancellation)?;
            let receipts = self.discover_source_catalog(source)?;
            for receipt in receipts {
                catalog_projects = catalog_projects.saturating_add(receipt.projects);
                catalog_sessions = catalog_sessions.saturating_add(receipt.sessions);
                if receipt.degraded {
                    degraded_sources.push(source.adapter_id.clone());
                }
            }
        }
        degraded_sources.sort();
        degraded_sources.dedup();

        let result = (|| {
            // Every watcher reaches its prepared boundary before history runs.
            // Dropping this vector tears all prepared workers down if any later
            // preparation fails.
            let mut prepared_supervisors = Vec::with_capacity(configured.len());
            for source in &configured {
                check_cancelled(&cancellation)?;
                let mut options = ObservationSupervisorOptions::new(source.roots.clone());
                options.reason.clone_from(&source.reason);
                prepared_supervisors.push(self.prepare_registered_observation_cancellable(
                    &source.adapter_id,
                    options,
                    cancellation.clone(),
                )?);
            }

            let background_cancellation = QueryCancellationToken::default();
            check_cancelled(&cancellation)?;
            let supervisors_started = prepared_supervisors.len();
            let mut starting = Vec::with_capacity(supervisors_started);
            for supervisor in prepared_supervisors {
                check_cancelled(&cancellation)?;
                starting.push(supervisor.begin_with_cancellation(background_cancellation.clone())?);
            }
            check_cancelled(&cancellation)?;
            let adapter_ids = configured
                .iter()
                .map(|source| source.adapter_id.clone())
                .collect::<BTreeSet<_>>();
            self.start_configured_observation_background(
                adapter_ids,
                starting,
                background_cancellation,
            )?;
            Ok(ConfiguredObservationStartupOutcome {
                catalog_projects,
                catalog_sessions,
                degraded_sources: degraded_sources.clone(),
                supervisors_started,
                history_background: true,
            })
        })();

        if result.is_err() {
            let _ = self.clear_configured_observation_startup();
        }
        result
    }

    /// Rescan one adapter's catalog. Called on engine open, on an explicit
    /// refresh, and after a supervisor drain that changed the native file set,
    /// which is what makes the catalog track an mtime change.
    pub(crate) fn discover_source_catalog(
        &self,
        source: &ConfiguredObservationSource,
    ) -> Result<Vec<super::catalog::CatalogScanReceipt>, EngineError> {
        let adapter = self.registered_adapter(&source.adapter_id)?;
        self.retain_configured_catalog_source(source.clone());
        let observed_at = now_ms();
        let specs = adapter
            .discover(&DiscoveryContext {
                configured_roots: source.roots.clone(),
                observed_at,
            })
            .map_err(|error| EngineError::InvalidConfig(error.to_string()))?;

        let mut receipts = Vec::with_capacity(specs.len());
        for spec in specs {
            spec.validate()
                .map_err(|error| EngineError::InvalidConfig(error.to_string()))?;
            let source_instance_id = self.reserve_source_instance(source_instance_spec(
                adapter.manifest(),
                &spec,
                observed_at,
                observed_at,
            ))?;
            let instance = SourceInstance {
                id: source_instance_id,
                spec,
            };
            let scan = scan_source(&adapter, &instance, &CatalogDiscoveryLimits::default())
                .unwrap_or_else(|error| {
                    SourceScan::degraded(
                        source_instance_id,
                        &source.adapter_id,
                        format!("catalog discovery failed: {error}"),
                    )
                });
            receipts.push(self.commit_catalog_scan(scan)?);
        }
        Ok(receipts)
    }

    /// Rescan every configured adapter that matches `adapter_id`, or all of
    /// them when it is `None`.
    pub(crate) fn rescan_catalog(&self, adapter_id: Option<&str>) -> Result<(), EngineError> {
        let sources = self.configured_catalog_sources();
        for source in sources {
            if adapter_id.is_some_and(|wanted| wanted != source.adapter_id) {
                continue;
            }
            self.discover_source_catalog(&source)?;
        }
        Ok(())
    }

    fn ensure_configured_observation_startup_available(&self) -> Result<(), EngineError> {
        if self
            .configured_observation_startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
        {
            return Err(EngineError::InvalidConfig(
                "configured observation startup is already installed".to_string(),
            ));
        }
        Ok(())
    }

    fn start_configured_observation_background(
        self: &Arc<Self>,
        adapter_ids: BTreeSet<String>,
        starting: Vec<StartingObservationSupervisor>,
        cancellation: QueryCancellationToken,
    ) -> Result<(), EngineError> {
        if adapter_ids.is_empty() || adapter_ids.len() != starting.len() {
            return Err(EngineError::InvalidConfig(
                "configured observation startup requires one unique adapter per supervisor"
                    .to_string(),
            ));
        }
        let lifecycle = self.lock_lifecycle();
        if lifecycle.phase != LifecyclePhase::Running {
            return Err(EngineError::ShuttingDown);
        }
        let mut installed = self
            .configured_observation_startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if installed.is_some() {
            return Err(EngineError::InvalidConfig(
                "configured observation startup is already installed".to_string(),
            ));
        }
        let runtime = ConfiguredObservationStartupRuntime::start(
            Arc::downgrade(self),
            adapter_ids,
            starting,
            cancellation,
        )?;
        *installed = Some(runtime);
        Ok(())
    }

    pub(super) fn configured_observation_startup_status(
        &self,
    ) -> Option<ConfiguredObservationStartupStatus> {
        self.configured_observation_startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(ConfiguredObservationStartupRuntime::status)
    }

    pub(super) fn wait_for_configured_observation_startup(&self) -> Result<(), EngineError> {
        let shared = self
            .configured_observation_startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.shared));
        let Some(shared) = shared else {
            return Ok(());
        };
        let progress = shared.wait();
        match progress.state {
            ConfiguredObservationStartupState::Installed => Ok(()),
            ConfiguredObservationStartupState::Starting => {
                unreachable!("wait returns a terminal state")
            }
            // Report what actually failed. Callers previously saw only
            // "configured observation startup worker is unavailable", which
            // hid a rejected fact commit behind a message about liveness.
            ConfiguredObservationStartupState::Failed => Err(EngineError::WorkerFailed {
                worker: "configured observation startup",
                detail: progress
                    .failure
                    .unwrap_or_else(|| "startup reported no detail".to_string()),
            }),
            ConfiguredObservationStartupState::Stopped => Err(EngineError::ShuttingDown),
        }
    }

    pub(super) fn clear_configured_observation_startup(&self) -> Result<(), EngineError> {
        let mut runtime = self
            .configured_observation_startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        match runtime.as_mut() {
            Some(runtime) => runtime.shutdown(),
            None => Ok(()),
        }
    }

    pub(super) fn clear_configured_observation_startup_for_adapter(
        &self,
        adapter_id: &str,
    ) -> Result<bool, EngineError> {
        let mut installed = self
            .configured_observation_startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !installed
            .as_ref()
            .is_some_and(|runtime| runtime.adapter_ids.contains(adapter_id))
        {
            return Ok(false);
        }
        let mut runtime = installed.take();
        drop(installed);
        if let Some(runtime) = runtime.as_mut() {
            runtime.shutdown()?;
        }
        Ok(true)
    }
}

fn normalize_configured_sources(
    configured: Vec<ConfiguredObservationSource>,
    cancellation: &QueryCancellationToken,
) -> Result<Vec<ConfiguredObservationSource>, EngineError> {
    check_cancelled(cancellation)?;
    if configured.is_empty() || configured.len() > MAX_CONFIGURED_OBSERVATION_SOURCES {
        return Err(EngineError::InvalidConfig(
            "configured observation requires a bounded nonempty source set".to_string(),
        ));
    }
    let mut adapter_ids = BTreeSet::new();
    let mut normalized = Vec::with_capacity(configured.len());
    for source in configured {
        check_cancelled(cancellation)?;
        AdapterId::new(source.adapter_id.as_str()).map_err(|_| {
            EngineError::InvalidConfig("configured adapter ID is invalid".to_string())
        })?;
        if !adapter_ids.insert(source.adapter_id.clone()) {
            return Err(EngineError::InvalidConfig(
                "configured observation contains a duplicate adapter".to_string(),
            ));
        }
        validate_request(&ReconcileRequest {
            configured_roots: source.roots.clone(),
            reason: source.reason.clone(),
        })?;
        let mut roots = BTreeSet::new();
        for root in source.roots {
            check_cancelled(cancellation)?;
            let canonical = root.canonicalize().map_err(|_| {
                EngineError::InvalidConfig(
                    "configured observation source root is unavailable".to_string(),
                )
            })?;
            roots.insert(canonical);
        }
        let roots = roots.into_iter().collect::<Vec<_>>();
        validate_request(&ReconcileRequest {
            configured_roots: roots.clone(),
            reason: source.reason.clone(),
        })?;
        normalized.push(ConfiguredObservationSource {
            adapter_id: source.adapter_id,
            roots,
            reason: source.reason,
        });
    }
    Ok(normalized)
}

fn check_cancelled(cancellation: &QueryCancellationToken) -> Result<(), EngineError> {
    if cancellation.is_cancelled() {
        Err(EngineError::QueryCancelled)
    } else {
        Ok(())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
