//! RFC 012B global startup ordering for configured durable sources.
//!
//! The sole owner validates the complete source set, registers every instance,
//! freezes catalog authority, installs every watcher, and only then permits a
//! catalog read or releases any full-history scan. The module is adapter-
//! neutral and retains the legacy history path when catalog authority is not
//! yet promoted.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::adapter::AdapterId;
use crate::catalog_contract::CatalogAccessPolicyDigest;

use super::catalog_build::{
    CatalogBuildIntent, CatalogBuildOutcome, CatalogBuildPreparation, CatalogConfiguredSource,
    PreparedCatalogBuild,
};
use super::catalog_refresh::{CATALOG_REFRESH_RETRY_DELAY, MAX_AUTOMATIC_REFRESH_ATTEMPTS};
use super::coordinator::validate_request;
use super::supervisor::StartingObservationSupervisor;
use super::{
    EngineError, LifecyclePhase, ObservationSupervisorOptions, QueryCancellationToken,
    ReconcileRequest, SpaghettiEngineCore,
};

const MAX_CONFIGURED_OBSERVATION_SOURCES: usize = 64;
const WITHHELD_CATALOG_POLICY: &[u8] = b"spaghetti/rfc012b/catalog-policy/withheld-v1";

#[derive(Debug, Clone)]
pub(crate) struct ConfiguredObservationSource {
    adapter_id: String,
    roots: Vec<PathBuf>,
    reason: String,
}

impl ConfiguredObservationSource {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfiguredCatalogStartupOutcome {
    Catalog(CatalogBuildOutcome),
    WatcherUnavailable { adapter_ids: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredObservationStartupOutcome {
    pub(crate) catalog: ConfiguredCatalogStartupOutcome,
    pub(crate) supervisors_started: usize,
    pub(crate) history_background: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfiguredObservationStartupState {
    Starting,
    Installed,
    Failed,
    Stopped,
}

struct ConfiguredObservationStartupShared {
    state: Mutex<ConfiguredObservationStartupState>,
    finished: Condvar,
}

impl ConfiguredObservationStartupShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ConfiguredObservationStartupState::Starting),
            finished: Condvar::new(),
        })
    }

    fn set(&self, state: ConfiguredObservationStartupState) {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
        self.finished.notify_all();
    }

    fn get(&self) -> ConfiguredObservationStartupState {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait(&self) -> ConfiguredObservationStartupState {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *state == ConfiguredObservationStartupState::Starting {
            state = self
                .finished
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *state
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
        let failure_engine = engine.clone();
        let join = thread::Builder::new()
            .name("spaghetti-configured-observation-startup".to_string())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    finish_configured_observation_startup(engine, starting, &thread_cancellation)
                }));
                let state = match result {
                    Ok(Ok(())) => ConfiguredObservationStartupState::Installed,
                    Ok(Err(EngineError::QueryCancelled | EngineError::ShuttingDown))
                        if thread_cancellation.is_cancelled() =>
                    {
                        ConfiguredObservationStartupState::Stopped
                    }
                    Ok(Err(_)) | Err(_) => {
                        if let Some(engine) = failure_engine.upgrade() {
                            let _ = engine.clear_configured_catalog_refresh();
                        }
                        ConfiguredObservationStartupState::Failed
                    }
                };
                thread_shared.set(state);
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
            self.shared.set(ConfiguredObservationStartupState::Stopped);
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
        match shared {
            Some(shared) => match shared.wait() {
                ConfiguredObservationStartupState::Installed => Ok(()),
                ConfiguredObservationStartupState::Starting => {
                    unreachable!("wait returns terminal state")
                }
                ConfiguredObservationStartupState::Failed => Err(EngineError::WorkerUnavailable {
                    worker: "configured observation startup",
                }),
                ConfiguredObservationStartupState::Stopped => Err(EngineError::ShuttingDown),
            },
            None => Ok(()),
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

    /// Start the complete configured durable host through the RFC 012B global
    /// planning barrier. Catalog publication is withheld when any prepared
    /// watcher backend is unavailable; history still starts under the existing
    /// authoritative polling fallback.
    pub(crate) fn start_configured_observation_cancellable(
        self: &Arc<Self>,
        configured: Vec<ConfiguredObservationSource>,
        cancellation: QueryCancellationToken,
    ) -> Result<ConfiguredObservationStartupOutcome, EngineError> {
        self.ensure_configured_observation_startup_available()?;
        let configured = normalize_configured_sources(configured, &cancellation)?;
        let access_policy_digest = CatalogAccessPolicyDigest::derive(1, WITHHELD_CATALOG_POLICY)
            .expect("fixed withheld catalog policy material is valid");
        let catalog_sources = configured
            .iter()
            .map(|source| {
                CatalogConfiguredSource::new(
                    source.adapter_id.clone(),
                    source.roots.clone(),
                    access_policy_digest,
                )
            })
            .collect::<Vec<_>>();
        let refresh_sources = catalog_sources.clone();

        // Register and freeze catalog authority before any watcher thread can
        // begin a full-history scan.
        let registered =
            self.register_configured_catalog_sources(catalog_sources, cancellation.clone())?;
        let catalog_preparation = self.prepare_registered_catalog(
            registered,
            CatalogBuildIntent::Startup,
            cancellation.clone(),
        )?;

        // Install the all-source refresh coordinates before watcher callbacks
        // become observable. A native change during watcher preparation or
        // catalog publication is retained as pending and flushed only after a
        // promoted catalog has published successfully.
        let refresh_staged = matches!(&catalog_preparation, CatalogBuildPreparation::Prepared(_));
        if refresh_staged {
            self.stage_configured_catalog_refresh(refresh_sources)?;
        }

        let result = (|| {
            // Every watcher reaches its prepared boundary before catalog I/O
            // or history. Dropping this vector tears all prepared workers down
            // if any later preparation fails.
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

            let mut watcher_unavailable = prepared_supervisors
                .iter()
                .filter(|supervisor| !supervisor.watcher_available())
                .map(|supervisor| supervisor.adapter_id().to_string())
                .collect::<Vec<_>>();
            watcher_unavailable.sort();
            let catalog = match catalog_preparation {
                CatalogBuildPreparation::AuthorizationUnavailable { adapter_ids } => {
                    ConfiguredCatalogStartupOutcome::Catalog(
                        CatalogBuildOutcome::AuthorizationUnavailable { adapter_ids },
                    )
                }
                CatalogBuildPreparation::Prepared(prepared) if watcher_unavailable.is_empty() => {
                    ConfiguredCatalogStartupOutcome::Catalog(publish_startup_catalog_with_retry(
                        self,
                        prepared,
                        cancellation.clone(),
                        CATALOG_REFRESH_RETRY_DELAY,
                        MAX_AUTOMATIC_REFRESH_ATTEMPTS,
                    )?)
                }
                CatalogBuildPreparation::Prepared(_) => {
                    ConfiguredCatalogStartupOutcome::WatcherUnavailable {
                        adapter_ids: watcher_unavailable,
                    }
                }
            };

            let refresh_enabled = matches!(
                &catalog,
                ConfiguredCatalogStartupOutcome::Catalog(
                    CatalogBuildOutcome::LastCompleteRetained
                        | CatalogBuildOutcome::InitialSourceUnavailable { .. }
                        | CatalogBuildOutcome::Published { .. }
                )
            );

            if refresh_enabled {
                let background_cancellation = QueryCancellationToken::default();
                check_cancelled(&cancellation)?;
                let supervisors_started = prepared_supervisors.len();
                let mut starting = Vec::with_capacity(supervisors_started);
                for supervisor in prepared_supervisors {
                    check_cancelled(&cancellation)?;
                    starting
                        .push(supervisor.begin_with_cancellation(background_cancellation.clone())?);
                }
                check_cancelled(&cancellation)?;
                self.activate_configured_catalog_refresh()?;
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
                return Ok(ConfiguredObservationStartupOutcome {
                    catalog,
                    supervisors_started,
                    history_background: true,
                });
            }

            if refresh_staged {
                self.clear_configured_catalog_refresh()?;
            }
            check_cancelled(&cancellation)?;
            let mut starting = Vec::with_capacity(prepared_supervisors.len());
            for supervisor in prepared_supervisors {
                check_cancelled(&cancellation)?;
                starting.push(supervisor.begin()?);
            }
            let mut started = Vec::with_capacity(starting.len());
            for supervisor in starting {
                started.push(supervisor.finish()?);
            }
            check_cancelled(&cancellation)?;
            let supervisors_started = started.len();
            self.install_started_observation_supervisors(started)?;
            Ok(ConfiguredObservationStartupOutcome {
                catalog,
                supervisors_started,
                history_background: false,
            })
        })();

        if result.is_err() && refresh_staged {
            let _ = self.clear_configured_observation_startup();
            let _ = self.clear_configured_catalog_refresh();
        }
        result
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

fn publish_startup_catalog_with_retry(
    engine: &Arc<SpaghettiEngineCore>,
    prepared: PreparedCatalogBuild,
    cancellation: QueryCancellationToken,
    retry_delay: Duration,
    max_attempts: usize,
) -> Result<CatalogBuildOutcome, EngineError> {
    run_startup_catalog_retry_policy(
        max_attempts,
        || {
            check_cancelled(&cancellation)?;
            engine.publish_prepared_catalog(prepared.clone(), cancellation.clone())
        },
        || {
            engine
                .mark_active_catalog_source_retrying("catalog_source_initial_retrying")
                .map(|_| ())
        },
        || engine.degrade_active_catalog_source("catalog_source_initial_exhausted"),
        || wait_for_catalog_retry(&cancellation, retry_delay),
    )
}

fn run_startup_catalog_retry_policy<Publish, MarkRetry, Degrade, Wait>(
    max_attempts: usize,
    mut publish: Publish,
    mut mark_retry: MarkRetry,
    mut degrade: Degrade,
    mut wait: Wait,
) -> Result<CatalogBuildOutcome, EngineError>
where
    Publish: FnMut() -> Result<CatalogBuildOutcome, EngineError>,
    MarkRetry: FnMut() -> Result<(), EngineError>,
    Degrade: FnMut() -> Result<Option<u64>, EngineError>,
    Wait: FnMut() -> Result<(), EngineError>,
{
    if max_attempts == 0 {
        return Err(EngineError::InvalidConfig(
            "catalog startup retry policy requires at least one attempt".to_string(),
        ));
    }
    for attempt in 1..=max_attempts {
        match publish() {
            Ok(outcome) => return Ok(outcome),
            Err(EngineError::Observation { .. }) if attempt < max_attempts => {
                mark_retry()?;
                wait()?;
            }
            Err(EngineError::Observation { .. }) => {
                let commit_seq = degrade()?;
                return Ok(CatalogBuildOutcome::InitialSourceUnavailable { commit_seq });
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("nonzero catalog retry policy always returns from its bounded loop")
}

fn wait_for_catalog_retry(
    cancellation: &QueryCancellationToken,
    delay: Duration,
) -> Result<(), EngineError> {
    let deadline = Instant::now() + delay;
    loop {
        check_cancelled(cancellation)?;
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(());
        };
        thread::sleep(remaining.min(Duration::from_millis(25)));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    use crossbeam_channel::bounded;
    use rusqlite::Connection;
    use tempfile::TempDir;

    use crate::adapter::{
        AdapterError, AdapterErrorClass, AdapterManifest, AdapterObjectContext, AdapterRegistry,
        AgentAdapter, ConsistencyPolicy, DecodeContext, DecodeDisposition, DecoderId,
        DeletionPolicy, DiscoveryContext, DriverSpec, EntityScope, FactBatch, ObjectSelector,
        RawRetentionPolicy, SourceInstance, SourceInstanceKey, SourceInstanceSpec,
        SourceObjectDescriptor, SourceRoot, StreamAuthority, StreamId, StreamSpec,
    };
    use crate::engine::EngineOptions;
    use crate::source::{AppendDelimitedConfig, IngestPriority, SourceRecord};

    use super::*;

    #[test]
    fn cold_catalog_retry_policy_recovers_or_publishes_terminal_degraded_state() {
        let mut publish_calls = 0;
        let mut retry_marks = 0;
        let mut waits = 0;
        let mut degrades = 0;
        let recovered = run_startup_catalog_retry_policy(
            3,
            || {
                publish_calls += 1;
                if publish_calls < 3 {
                    Err(EngineError::Observation {
                        operation: "test cold catalog source",
                        detail: "retryable".to_string(),
                    })
                } else {
                    Ok(CatalogBuildOutcome::Published {
                        kind: crate::engine::catalog_build::CatalogPublicationKind::Initial,
                        commit_seq: Some(19),
                        source_count: 1,
                        member_count: 2,
                    })
                }
            },
            || {
                retry_marks += 1;
                Ok(())
            },
            || {
                degrades += 1;
                Ok(Some(20))
            },
            || {
                waits += 1;
                Ok(())
            },
        )
        .unwrap();
        assert!(matches!(
            recovered,
            CatalogBuildOutcome::Published {
                kind: crate::engine::catalog_build::CatalogPublicationKind::Initial,
                commit_seq: Some(19),
                ..
            }
        ));
        assert_eq!((publish_calls, retry_marks, waits, degrades), (3, 2, 2, 0));

        let mut publish_calls = 0;
        let mut retry_marks = 0;
        let mut waits = 0;
        let mut degrades = 0;
        let degraded = run_startup_catalog_retry_policy(
            3,
            || {
                publish_calls += 1;
                Err(EngineError::Observation {
                    operation: "test cold catalog source",
                    detail: "unavailable".to_string(),
                })
            },
            || {
                retry_marks += 1;
                Ok(())
            },
            || {
                degrades += 1;
                Ok(Some(29))
            },
            || {
                waits += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            degraded,
            CatalogBuildOutcome::InitialSourceUnavailable {
                commit_seq: Some(29)
            }
        );
        assert_eq!((publish_calls, retry_marks, waits, degrades), (3, 2, 2, 1));
    }

    #[test]
    fn cold_catalog_retry_policy_never_reclassifies_integrity_or_empty_policy() {
        let mut retry_marks = 0;
        let mut degrades = 0;
        let error = run_startup_catalog_retry_policy(
            3,
            || {
                Err(EngineError::CatalogIntegrity {
                    operation: "test cold catalog assembly",
                })
            },
            || {
                retry_marks += 1;
                Ok(())
            },
            || {
                degrades += 1;
                Ok(Some(1))
            },
            || Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, EngineError::CatalogIntegrity { .. }));
        assert_eq!((retry_marks, degrades), (0, 0));

        assert!(matches!(
            run_startup_catalog_retry_policy(
                0,
                || Ok(CatalogBuildOutcome::LastCompleteRetained),
                || Ok(()),
                || Ok(None),
                || Ok(()),
            ),
            Err(EngineError::InvalidConfig(_))
        ));
    }

    struct OrderedStartupAdapter {
        manifest: AdapterManifest,
        discoveries: Arc<AtomicUsize>,
        final_watcher_topology_discovered: Arc<AtomicBool>,
        marks_final_topology: bool,
        decode_gate: Option<Arc<DecodeGate>>,
    }

    impl OrderedStartupAdapter {
        fn new(
            adapter_id: &str,
            discoveries: Arc<AtomicUsize>,
            final_watcher_topology_discovered: Arc<AtomicBool>,
            marks_final_topology: bool,
        ) -> Self {
            Self {
                manifest: AdapterManifest {
                    id: AdapterId::new(adapter_id).unwrap(),
                    display_name: format!("{adapter_id} startup test adapter"),
                    adapter_version: "1.0.0".to_string(),
                    contract_version: 1,
                    support_binding: None,
                    scope_programs: None,
                    source_schema_versions: Vec::new(),
                    capabilities: Vec::new(),
                },
                discoveries,
                final_watcher_topology_discovered,
                marks_final_topology,
                decode_gate: None,
            }
        }

        fn with_decode_gate(mut self, decode_gate: Arc<DecodeGate>) -> Self {
            self.decode_gate = Some(decode_gate);
            self
        }
    }

    impl AgentAdapter for OrderedStartupAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            let call = self.discoveries.fetch_add(1, Ordering::AcqRel) + 1;
            if self.marks_final_topology && call == 2 {
                self.final_watcher_topology_discovered
                    .store(true, Ordering::Release);
            }
            context
                .configured_roots
                .iter()
                .map(|root| {
                    let canonical = root.canonicalize().map_err(|_| {
                        AdapterError::new(
                            AdapterErrorClass::Transient,
                            "root_unavailable",
                            "configured source root is unavailable",
                        )
                    })?;
                    let mut stable_key = self.manifest.id.as_str().as_bytes().to_vec();
                    stable_key.push(0);
                    stable_key.extend_from_slice(canonical.as_os_str().as_encoded_bytes());
                    Ok(SourceInstanceSpec {
                        identity_contract_version: 1,
                        stable_key: SourceInstanceKey::new(stable_key)?,
                        display_name: format!("{} source", self.manifest.id),
                        roots: vec![SourceRoot {
                            name: "root".to_string(),
                            path: canonical,
                        }],
                        discovery_reason: "configured startup test".to_string(),
                    })
                })
                .collect()
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(vec![StreamSpec {
                id: StreamId::new("events")?,
                driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
                selector: ObjectSelector {
                    root_name: "root".to_string(),
                    include: vec!["*.jsonl".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new("ordered-startup-v1")?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Instance,
                priority: IngestPriority::Backfill,
                consistency: ConsistencyPolicy::IncrementalCursor,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::HashOnly,
                capabilities: Vec::new(),
            }])
        }

        fn bootstrap_object(
            &self,
            _instance: &SourceInstance,
            _object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            Ok(AdapterObjectContext::empty())
        }

        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            if let Some(gate) = self.decode_gate.as_ref() {
                gate.enter();
            }
            assert!(
                self.final_watcher_topology_discovered
                    .load(Ordering::Acquire),
                "no full-history decode may begin before every watcher is prepared"
            );
            Ok(DecodeDisposition::IgnoredKnown)
        }
    }

    #[derive(Default)]
    struct DecodeGate {
        state: Mutex<DecodeGateState>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct DecodeGateState {
        blocked: bool,
        released: bool,
    }

    impl DecodeGate {
        fn enter(&self) {
            let mut state = self.state.lock().unwrap();
            if state.released {
                return;
            }
            state.blocked = true;
            self.changed.notify_all();
            while !state.released {
                state = self.changed.wait(state).unwrap();
            }
        }

        fn wait_until_entered(&self, timeout: Duration) {
            let deadline = Instant::now() + timeout;
            let mut state = self.state.lock().unwrap();
            while !state.blocked {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("background observation did not reach the decode gate");
                let (next, timed_out) = self.changed.wait_timeout(state, remaining).unwrap();
                state = next;
                assert!(!timed_out.timed_out(), "background decode timed out");
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.released = true;
            self.changed.notify_all();
        }
    }

    fn configured_root(directory: &TempDir, name: &str) -> PathBuf {
        let root = directory.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("events.jsonl"), b"{}\n").unwrap();
        root
    }

    #[test]
    fn all_sources_register_and_prepare_before_any_history_decode() {
        let directory = TempDir::new().unwrap();
        let alpha_calls = Arc::new(AtomicUsize::new(0));
        let beta_calls = Arc::new(AtomicUsize::new(0));
        let final_watcher_topology_discovered = Arc::new(AtomicBool::new(false));
        let registry = AdapterRegistry::builder()
            .register(OrderedStartupAdapter::new(
                "alpha",
                Arc::clone(&alpha_calls),
                Arc::clone(&final_watcher_topology_discovered),
                false,
            ))
            .register(OrderedStartupAdapter::new(
                "beta",
                Arc::clone(&beta_calls),
                Arc::clone(&final_watcher_topology_discovered),
                true,
            ))
            .build()
            .unwrap();
        let database_path = directory.path().join("startup.db");
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: database_path.clone(),
                query_workers: Some(1),
                owner_label: Some("configured-startup-test".to_string()),
                defer_query_structures: false,
                source_pass_pool: None,
            },
            registry,
        )
        .unwrap();

        let outcome = engine
            .start_configured_observation_cancellable(
                vec![
                    ConfiguredObservationSource::new(
                        "alpha",
                        vec![configured_root(&directory, "alpha")],
                        "configured_startup",
                    ),
                    ConfiguredObservationSource::new(
                        "beta",
                        vec![configured_root(&directory, "beta")],
                        "configured_startup",
                    ),
                ],
                QueryCancellationToken::default(),
            )
            .unwrap();
        assert_eq!(
            outcome,
            ConfiguredObservationStartupOutcome {
                catalog: ConfiguredCatalogStartupOutcome::Catalog(
                    CatalogBuildOutcome::AuthorizationUnavailable {
                        adapter_ids: vec!["alpha".to_string(), "beta".to_string()],
                    },
                ),
                supervisors_started: 2,
                history_background: false,
            }
        );
        assert!(alpha_calls.load(Ordering::Acquire) >= 3);
        assert!(beta_calls.load(Ordering::Acquire) >= 3);

        let connection = Connection::open(database_path).unwrap();
        let source_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        let object_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_objects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_count, 2);
        assert_eq!(object_count, 2);
        assert_eq!(engine.status().observation.supervisors_running, 2);
        assert!(
            !engine.configured_catalog_refresh_is_active(),
            "candidate sources must not start the promoted catalog refresh worker"
        );
        engine.shutdown().unwrap();
    }

    #[test]
    fn background_history_installs_after_return_and_bootstrap_waits_for_it() {
        const WAIT: Duration = Duration::from_secs(15);
        let directory = TempDir::new().unwrap();
        let discoveries = Arc::new(AtomicUsize::new(0));
        let final_watcher_topology_discovered = Arc::new(AtomicBool::new(true));
        let gate = Arc::new(DecodeGate::default());
        let registry = AdapterRegistry::builder()
            .register(
                OrderedStartupAdapter::new(
                    "alpha",
                    discoveries,
                    final_watcher_topology_discovered,
                    false,
                )
                .with_decode_gate(Arc::clone(&gate)),
            )
            .build()
            .unwrap();
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: directory.path().join("background-startup.db"),
                query_workers: Some(1),
                owner_label: Some("background-startup-test".to_string()),
                defer_query_structures: true,
                source_pass_pool: None,
            },
            registry,
        )
        .unwrap();
        let root = configured_root(&directory, "alpha");
        let request_cancellation = QueryCancellationToken::default();
        let background_cancellation = QueryCancellationToken::default();
        let prepared = engine
            .prepare_registered_observation_cancellable(
                "alpha",
                ObservationSupervisorOptions::new(vec![root]),
                request_cancellation.clone(),
            )
            .unwrap();
        let starting = prepared
            .begin_with_cancellation(background_cancellation.clone())
            .unwrap();
        engine
            .start_configured_observation_background(
                BTreeSet::from(["alpha".to_string()]),
                vec![starting],
                background_cancellation,
            )
            .unwrap();
        gate.wait_until_entered(WAIT);

        let status = engine.status();
        assert!(
            matches!(
                status.observation.state.as_str(),
                "scanning" | "reconciling"
            ),
            "background history must remain visibly in progress: {status:?}"
        );
        assert_eq!(status.observation.supervisors_running, 0);
        assert_eq!(
            engine.configured_observation_startup_status(),
            Some(ConfiguredObservationStartupStatus {
                active: true,
                failed: false,
            })
        );

        request_cancellation.cancel();
        let (completed_tx, completed_rx) = bounded(1);
        let bootstrap_engine = Arc::clone(&engine);
        let bootstrap = std::thread::spawn(move || {
            completed_tx
                .send(bootstrap_engine.complete_query_bootstrap())
                .unwrap();
        });
        assert!(
            completed_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "query bootstrap must wait for the background history batch"
        );

        gate.release();
        completed_rx.recv_timeout(WAIT).unwrap().unwrap();
        bootstrap.join().unwrap();
        assert_eq!(engine.status().observation.supervisors_running, 1);
        assert_eq!(
            engine.configured_observation_startup_status(),
            Some(ConfiguredObservationStartupStatus {
                active: false,
                failed: false,
            })
        );
        engine.shutdown().unwrap();
    }

    #[test]
    fn duplicate_configuration_fails_before_registration() {
        let directory = TempDir::new().unwrap();
        let discoveries = Arc::new(AtomicUsize::new(0));
        let final_watcher_topology_discovered = Arc::new(AtomicBool::new(false));
        let registry = AdapterRegistry::builder()
            .register(OrderedStartupAdapter::new(
                "alpha",
                Arc::clone(&discoveries),
                final_watcher_topology_discovered,
                false,
            ))
            .build()
            .unwrap();
        let database_path = directory.path().join("duplicate.db");
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: database_path.clone(),
                query_workers: Some(1),
                owner_label: Some("configured-startup-duplicate-test".to_string()),
                defer_query_structures: false,
                source_pass_pool: None,
            },
            registry,
        )
        .unwrap();
        let root = configured_root(&directory, "alpha");
        let result = engine.start_configured_observation_cancellable(
            vec![
                ConfiguredObservationSource::new("alpha", vec![root.clone()], "configured_startup"),
                ConfiguredObservationSource::new("alpha", vec![root], "configured_startup"),
            ],
            QueryCancellationToken::default(),
        );
        assert!(matches!(result, Err(EngineError::InvalidConfig(_))));
        assert_eq!(discoveries.load(Ordering::Acquire), 0);
        let connection = Connection::open(database_path).unwrap();
        let source_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(source_count, 0);
        engine.shutdown().unwrap();
    }
}
