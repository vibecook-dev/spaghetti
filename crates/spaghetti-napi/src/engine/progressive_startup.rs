//! RFC 012B global startup ordering for configured durable sources.
//!
//! The sole owner validates the complete source set, registers every instance,
//! freezes catalog authority, installs every watcher, and only then permits a
//! catalog read or releases any full-history scan. The module is adapter-
//! neutral and retains the legacy history path when catalog authority is not
//! yet promoted.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::adapter::AdapterId;
use crate::catalog_contract::CatalogAccessPolicyDigest;

use super::catalog_build::{
    CatalogBuildIntent, CatalogBuildOutcome, CatalogBuildPreparation, CatalogConfiguredSource,
};
use super::coordinator::validate_request;
use super::{
    EngineError, ObservationSupervisorOptions, QueryCancellationToken, ReconcileRequest,
    SpaghettiEngineCore,
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
}

impl SpaghettiEngineCore {
    /// Start the complete configured durable host through the RFC 012B global
    /// planning barrier. Catalog publication is withheld when any prepared
    /// watcher backend is unavailable; history still starts under the existing
    /// authoritative polling fallback.
    pub(crate) fn start_configured_observation_cancellable(
        self: &Arc<Self>,
        configured: Vec<ConfiguredObservationSource>,
        cancellation: QueryCancellationToken,
    ) -> Result<ConfiguredObservationStartupOutcome, EngineError> {
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
            .collect();

        // Register and freeze catalog authority before any watcher thread can
        // begin a full-history scan.
        let registered =
            self.register_configured_catalog_sources(catalog_sources, cancellation.clone())?;
        let catalog_preparation = self.prepare_registered_catalog(
            registered,
            CatalogBuildIntent::Startup,
            cancellation.clone(),
        )?;

        // Every watcher reaches its prepared boundary before catalog I/O or
        // history. Dropping this vector tears all prepared workers down if any
        // later preparation fails.
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
                ConfiguredCatalogStartupOutcome::Catalog(
                    self.publish_prepared_catalog(prepared, cancellation.clone())?,
                )
            }
            CatalogBuildPreparation::Prepared(_) => {
                ConfiguredCatalogStartupOutcome::WatcherUnavailable {
                    adapter_ids: watcher_unavailable,
                }
            }
        };

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
        })
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

    struct OrderedStartupAdapter {
        manifest: AdapterManifest,
        discoveries: Arc<AtomicUsize>,
        final_watcher_topology_discovered: Arc<AtomicBool>,
        marks_final_topology: bool,
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
            }
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
            assert!(
                self.final_watcher_topology_discovered
                    .load(Ordering::Acquire),
                "no full-history decode may begin before every watcher is prepared"
            );
            Ok(DecodeDisposition::IgnoredKnown)
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
