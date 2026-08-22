//! Engine-owned RFC 012B catalog build orchestration.
//!
//! This module joins already-reviewed authority, source-composition, projection,
//! and durable-publication seams. It deliberately has no public transport: a
//! configured source without promoted catalog authority returns an explicit
//! non-authorizing outcome and cannot cause catalog reads or a false Ready
//! publication.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::adapter::{ContractVersionSelection, SourceCoverageSet, SourceInstance};
use crate::catalog_contract::{
    CatalogAccessPolicyDigest, CatalogCoveragePlan, CatalogCoveragePlanSource,
    CatalogCoverageScope, CatalogReadinessPhase,
};
use crate::source::catalog_composition::{CatalogCompositionError, CatalogCompositionFailureClass};
use crate::source::catalog_projection::{
    CatalogInitialProjectionBatch, CatalogRefreshProjectionBatch, CatalogSourceProjection,
};
use crate::source::catalog_runtime_registry::CatalogSourceRuntime;
use crate::source::IngestPriority;

use super::{EngineError, ObservationCoordinator, QueryCancellationToken, SpaghettiEngineCore};

#[derive(Debug, Clone)]
pub(crate) struct CatalogConfiguredSource {
    adapter_id: String,
    configured_roots: Vec<PathBuf>,
    access_policy_digest: CatalogAccessPolicyDigest,
}

impl CatalogConfiguredSource {
    pub(crate) fn new(
        adapter_id: impl Into<String>,
        configured_roots: Vec<PathBuf>,
        access_policy_digest: CatalogAccessPolicyDigest,
    ) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            configured_roots,
            access_policy_digest,
        }
    }

    pub(crate) fn adapter_id(&self) -> &str {
        &self.adapter_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogBuildIntent {
    Startup,
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogPublicationKind {
    Initial,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogBuildOutcome {
    /// Every instance is registered, but at least one configured adapter has
    /// no promoted catalog authority. No catalog source object was read.
    AuthorizationUnavailable { adapter_ids: Vec<String> },
    /// Startup retained the restart-authenticated last complete publication
    /// without manufacturing a refresh or consuming native catalog sources.
    LastCompleteRetained,
    Published {
        kind: CatalogPublicationKind,
        commit_seq: Option<u64>,
        source_count: usize,
        member_count: usize,
    },
}

struct PreparedCatalogSource {
    instance: SourceInstance,
    authorization: crate::adapter::TypedAccessAuthorization,
    access_policy_digest: CatalogAccessPolicyDigest,
    runtime: Arc<dyn CatalogSourceRuntime>,
}

struct RegisteredCatalogSource {
    configured: CatalogConfiguredSource,
    instances: Vec<SourceInstance>,
}

pub(crate) struct RegisteredCatalogSources {
    sources: Vec<RegisteredCatalogSource>,
}

pub(crate) enum CatalogBuildPreparation {
    AuthorizationUnavailable { adapter_ids: Vec<String> },
    Prepared(PreparedCatalogBuild),
}

pub(crate) struct PreparedCatalogBuild {
    plan: CatalogCoveragePlan,
    selection: ContractVersionSelection,
    sources: Vec<PreparedCatalogSource>,
    intent: CatalogBuildIntent,
}

impl SpaghettiEngineCore {
    /// Register the complete configured source set, obtain catalog-only typed
    /// authority for every required source, freeze the normalized Library plan,
    /// and only then read bounded native catalog objects and publish atomically.
    ///
    /// `Startup` retains an already-safe warm publication. `Refresh` explicitly
    /// starts (or resumes) an ordinary same-plan refresh. A changed plan remains
    /// fail-closed until the durable plan-transition contract is implemented.
    pub(crate) fn reconcile_configured_catalog(
        self: &Arc<Self>,
        configured: Vec<CatalogConfiguredSource>,
        intent: CatalogBuildIntent,
        cancellation: QueryCancellationToken,
    ) -> Result<CatalogBuildOutcome, EngineError> {
        let priority = match intent {
            CatalogBuildIntent::Startup => IngestPriority::Backfill,
            CatalogBuildIntent::Refresh => IngestPriority::ForegroundRepair,
        };
        let _source_pass = self.acquire_source_pass(priority)?;
        let registered =
            self.register_configured_catalog_sources(configured, cancellation.clone())?;
        match self.prepare_registered_catalog(registered, intent, cancellation.clone())? {
            CatalogBuildPreparation::AuthorizationUnavailable { adapter_ids } => {
                Ok(CatalogBuildOutcome::AuthorizationUnavailable { adapter_ids })
            }
            CatalogBuildPreparation::Prepared(prepared) => {
                self.publish_prepared_catalog(prepared, cancellation)
            }
        }
    }

    /// Discover and durably register the entire configured source set. This
    /// phase performs no support probe, catalog source access, or publication.
    pub(crate) fn register_configured_catalog_sources(
        self: &Arc<Self>,
        configured: Vec<CatalogConfiguredSource>,
        cancellation: QueryCancellationToken,
    ) -> Result<RegisteredCatalogSources, EngineError> {
        validate_configured_sources(&configured, &cancellation)?;
        let coordinator =
            ObservationCoordinator::with_cancellation(Arc::clone(self), cancellation.clone());
        let mut sources = Vec::with_capacity(configured.len());
        for configured in configured {
            check_cancelled(&cancellation)?;
            let adapter = self.registered_adapter(&configured.adapter_id)?;
            let instances = coordinator.discover_and_register_sources(
                adapter.as_ref(),
                configured.configured_roots.clone(),
            )?;
            sources.push(RegisteredCatalogSource {
                configured,
                instances,
            });
        }
        Ok(RegisteredCatalogSources { sources })
    }

    /// Require catalog authority for every registered source and freeze the
    /// normalized plan. Runtime source capabilities are not resolved until all
    /// required authorities exist, and this phase performs no catalog read.
    pub(crate) fn prepare_registered_catalog(
        &self,
        registered: RegisteredCatalogSources,
        intent: CatalogBuildIntent,
        cancellation: QueryCancellationToken,
    ) -> Result<CatalogBuildPreparation, EngineError> {
        check_cancelled(&cancellation)?;
        let mut unavailable = Vec::new();
        let mut authorized_sources = Vec::with_capacity(registered.sources.len());
        for source in registered.sources {
            check_cancelled(&cancellation)?;
            let Some(authorization) = self.catalog_authorization_for_roots(
                &source.configured.adapter_id,
                &source.configured.configured_roots,
            )?
            else {
                unavailable.push(source.configured.adapter_id);
                continue;
            };
            authorized_sources.push((source, authorization));
        }
        if !unavailable.is_empty() {
            unavailable.sort();
            return Ok(CatalogBuildPreparation::AuthorizationUnavailable {
                adapter_ids: unavailable,
            });
        }

        let mut authorized = Vec::new();
        for (source, authorization) in authorized_sources {
            let runtime = self
                .catalog_source_runtimes
                .resolve(&source.configured.adapter_id)
                .map_err(|error| {
                    catalog_composition_error("resolve catalog source runtime", error)
                })?;
            for instance in source.instances {
                authorized.push(PreparedCatalogSource {
                    instance,
                    authorization: authorization.clone(),
                    access_policy_digest: source.configured.access_policy_digest,
                    runtime: Arc::clone(&runtime),
                });
            }
        }
        if authorized.is_empty() {
            return Err(EngineError::InvalidConfig(
                "catalog build produced no authorized source instances".to_string(),
            ));
        }

        let selection = common_contract_selection(&authorized)?;
        let mut plan_sources = Vec::with_capacity(authorized.len());
        for source in &authorized {
            check_cancelled(&cancellation)?;
            plan_sources.push(derive_plan_source(source).map_err(|_| {
                EngineError::InvalidCommit(
                    "configured catalog source does not match its bound composition".to_string(),
                )
            })?);
        }
        let plan =
            CatalogCoveragePlan::new(CatalogCoverageScope::Library, plan_sources, Vec::new())
                .map_err(|_| catalog_integrity_error("freeze catalog coverage plan"))?;

        let durable_state = self.load_catalog_build_state()?;
        if durable_state
            .as_ref()
            .is_some_and(|state| state.plan != plan)
        {
            return Err(EngineError::InvalidCommit(
                "configured catalog source set does not match the durable frozen plan".to_string(),
            ));
        }

        Ok(CatalogBuildPreparation::Prepared(PreparedCatalogBuild {
            plan,
            selection,
            sources: authorized,
            intent,
        }))
    }

    /// Publish an already-frozen build only after the host has crossed its
    /// watcher-installation barrier. This is the first phase allowed to read a
    /// catalog source object.
    pub(crate) fn publish_prepared_catalog(
        self: &Arc<Self>,
        prepared: PreparedCatalogBuild,
        cancellation: QueryCancellationToken,
    ) -> Result<CatalogBuildOutcome, EngineError> {
        check_cancelled(&cancellation)?;
        let PreparedCatalogBuild {
            plan,
            selection,
            sources,
            intent,
        } = prepared;
        let durable_state = self.load_catalog_build_state()?;
        if durable_state
            .as_ref()
            .is_some_and(|state| state.plan != plan)
        {
            return Err(EngineError::InvalidCommit(
                "configured catalog source set does not match the durable frozen plan".to_string(),
            ));
        }
        match durable_state.as_ref().map(|state| state.readiness.state) {
            None | Some(CatalogReadinessPhase::Pending) => {
                publish_initial(self, plan, selection, &sources, &cancellation)
            }
            Some(CatalogReadinessPhase::Building)
                if durable_state
                    .as_ref()
                    .and_then(|state| state.readiness.last_complete_snapshot)
                    .is_some() =>
            {
                publish_refresh(self, plan, selection, &sources, &cancellation)
            }
            Some(CatalogReadinessPhase::Building) => {
                publish_initial(self, plan, selection, &sources, &cancellation)
            }
            Some(CatalogReadinessPhase::Ready) => {
                let refreshing = durable_state
                    .as_ref()
                    .and_then(|state| state.readiness.refreshing_from_snapshot)
                    .is_some();
                if intent == CatalogBuildIntent::Startup && !refreshing {
                    Ok(CatalogBuildOutcome::LastCompleteRetained)
                } else {
                    publish_refresh(self, plan, selection, &sources, &cancellation)
                }
            }
            Some(CatalogReadinessPhase::Error)
                if intent == CatalogBuildIntent::Startup
                    && durable_state
                        .as_ref()
                        .and_then(|state| state.readiness.last_complete_snapshot)
                        .is_some() =>
            {
                Ok(CatalogBuildOutcome::LastCompleteRetained)
            }
            Some(CatalogReadinessPhase::Error)
                if durable_state
                    .as_ref()
                    .and_then(|state| state.readiness.last_complete_snapshot)
                    .is_none() =>
            {
                publish_initial(self, plan, selection, &sources, &cancellation)
            }
            Some(CatalogReadinessPhase::Error) => {
                publish_refresh(self, plan, selection, &sources, &cancellation)
            }
            Some(CatalogReadinessPhase::Degraded) if intent == CatalogBuildIntent::Startup => {
                Ok(CatalogBuildOutcome::LastCompleteRetained)
            }
            Some(CatalogReadinessPhase::Degraded) => {
                publish_refresh(self, plan, selection, &sources, &cancellation)
            }
            Some(_) => Err(EngineError::InvalidCommit(
                "catalog build cannot resume from the durable readiness phase".to_string(),
            )),
        }
    }
}

fn validate_configured_sources(
    configured: &[CatalogConfiguredSource],
    cancellation: &QueryCancellationToken,
) -> Result<(), EngineError> {
    check_cancelled(cancellation)?;
    if configured.is_empty() {
        return Err(EngineError::InvalidConfig(
            "catalog build requires at least one configured source".to_string(),
        ));
    }
    let mut adapter_ids = BTreeSet::new();
    for source in configured {
        crate::adapter::AdapterId::new(source.adapter_id.as_str())
            .map_err(|error| EngineError::InvalidConfig(error.to_string()))?;
        if !adapter_ids.insert(source.adapter_id.clone()) {
            return Err(EngineError::InvalidConfig(
                "catalog build contains a duplicate configured adapter".to_string(),
            ));
        }
    }
    Ok(())
}

fn publish_initial(
    engine: &Arc<SpaghettiEngineCore>,
    plan: CatalogCoveragePlan,
    selection: ContractVersionSelection,
    sources: &[PreparedCatalogSource],
    cancellation: &QueryCancellationToken,
) -> Result<CatalogBuildOutcome, EngineError> {
    // This durable transition is intentionally before the first producer read:
    // a crash or cancellation leaves an exact resumable Building lineage.
    let context = engine.begin_initial_catalog_build(plan)?;
    let outcome = (|| {
        check_cancelled(cancellation)?;
        let projections = sources
            .iter()
            .map(|source| {
                check_cancelled(cancellation)?;
                produce_projection(source, None)
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
        let batch = CatalogInitialProjectionBatch::assemble(
            projections,
            selection,
            context.observation_commit(),
        )
        .map_err(|_| catalog_integrity_error("assemble initial catalog projection"))?;
        let source_count = batch.source_count();
        let member_count = batch.member_count();
        check_cancelled(cancellation)?;
        let receipt = engine.commit_initial_catalog_projection(context, batch)?;
        Ok(CatalogBuildOutcome::Published {
            kind: CatalogPublicationKind::Initial,
            commit_seq: receipt.map(|value| value.commit_seq),
            source_count,
            member_count,
        })
    })();
    if matches!(&outcome, Err(EngineError::CatalogIntegrity { .. })) {
        engine.fail_active_initial_catalog_integrity("catalog_initial_integrity_failed")?;
    }
    outcome
}

fn publish_refresh(
    engine: &Arc<SpaghettiEngineCore>,
    plan: CatalogCoveragePlan,
    selection: ContractVersionSelection,
    sources: &[PreparedCatalogSource],
    cancellation: &QueryCancellationToken,
) -> Result<CatalogBuildOutcome, EngineError> {
    let context = engine.begin_catalog_refresh(plan)?;
    check_cancelled(cancellation)?;
    let mut projections = Vec::with_capacity(sources.len());
    for source in sources {
        check_cancelled(cancellation)?;
        let plan_source = derive_plan_source(source)
            .map_err(|error| catalog_composition_error("bind catalog plan source", error))?;
        let prior = context
            .prior_source_coverage(&plan_source)
            .map_err(|_| catalog_integrity_error("bind prior catalog source coverage"))?;
        projections.push(produce_projection(source, Some(prior))?);
    }
    let batch = CatalogRefreshProjectionBatch::assemble(
        projections,
        selection,
        context.observation_commit(),
        context.prior_reducer(),
    )
    .map_err(|_| catalog_integrity_error("assemble catalog refresh projection"))?;
    let source_count = batch.source_count();
    let member_count = batch.member_count();
    check_cancelled(cancellation)?;
    let receipt = engine.commit_catalog_refresh_projection(context, batch)?;
    Ok(CatalogBuildOutcome::Published {
        kind: CatalogPublicationKind::Refresh,
        commit_seq: receipt.map(|value| value.commit_seq),
        source_count,
        member_count,
    })
}

fn common_contract_selection(
    sources: &[PreparedCatalogSource],
) -> Result<ContractVersionSelection, EngineError> {
    let selection = sources
        .first()
        .expect("authorized source count checked")
        .authorization
        .contracts()
        .clone();
    if sources
        .iter()
        .any(|source| source.authorization.contracts() != &selection)
    {
        return Err(EngineError::InvalidConfig(
            "configured catalog sources negotiated different contract selections".to_string(),
        ));
    }
    Ok(selection)
}

fn derive_plan_source(
    source: &PreparedCatalogSource,
) -> Result<CatalogCoveragePlanSource, CatalogCompositionError> {
    source.runtime.library_plan_source(
        &source.authorization,
        &source.instance,
        source.access_policy_digest,
    )
}

fn produce_projection(
    source: &PreparedCatalogSource,
    prior: Option<&SourceCoverageSet>,
) -> Result<CatalogSourceProjection, EngineError> {
    source
        .runtime
        .produce_library_projection(
            &source.authorization,
            &source.instance,
            source.access_policy_digest,
            prior,
        )
        .map_err(|error| catalog_composition_error("produce catalog source", error))
}

fn check_cancelled(cancellation: &QueryCancellationToken) -> Result<(), EngineError> {
    if cancellation.is_cancelled() {
        Err(EngineError::QueryCancelled)
    } else {
        Ok(())
    }
}

fn catalog_build_error(operation: &'static str, error: impl std::fmt::Display) -> EngineError {
    EngineError::Observation {
        operation,
        detail: error.to_string(),
    }
}

fn catalog_integrity_error(operation: &'static str) -> EngineError {
    EngineError::CatalogIntegrity { operation }
}

fn catalog_composition_error(
    operation: &'static str,
    error: CatalogCompositionError,
) -> EngineError {
    match error.class() {
        CatalogCompositionFailureClass::SourceUnavailable => catalog_build_error(operation, error),
        CatalogCompositionFailureClass::Integrity => catalog_integrity_error(operation),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use rusqlite::Connection;
    use tempfile::TempDir;

    use crate::adapter::{
        AdapterError, AdapterId, AdapterManifest, AdapterObjectContext, AdapterRegistry,
        AgentAdapter, DecodeContext, DecodeDisposition, DiscoveryContext, FactBatch,
        SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor, SourceRoot, StreamSpec,
    };
    use crate::engine::EngineOptions;
    use crate::source::SourceRecord;

    use super::*;

    struct RegistrationOnlyAdapter {
        manifest: AdapterManifest,
    }

    impl RegistrationOnlyAdapter {
        fn new(adapter_id: &str) -> Self {
            Self {
                manifest: AdapterManifest {
                    id: AdapterId::new(adapter_id).unwrap(),
                    display_name: adapter_id.to_string(),
                    adapter_version: "1.0.0".to_string(),
                    contract_version: 1,
                    support_binding: None,
                    scope_programs: None,
                    source_schema_versions: Vec::new(),
                    capabilities: Vec::new(),
                },
            }
        }
    }

    impl AgentAdapter for RegistrationOnlyAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            context
                .configured_roots
                .iter()
                .map(|root| {
                    let mut stable_key = self.manifest.id.as_str().as_bytes().to_vec();
                    stable_key.push(0);
                    stable_key.extend_from_slice(root.as_os_str().as_encoded_bytes());
                    Ok(SourceInstanceSpec {
                        identity_contract_version: 1,
                        stable_key: SourceInstanceKey::new(stable_key)?,
                        display_name: format!("{} test source", self.manifest.id),
                        roots: vec![SourceRoot {
                            name: "root".to_string(),
                            path: root.clone(),
                        }],
                        discovery_reason: "catalog registration test".to_string(),
                    })
                })
                .collect()
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(Vec::new())
        }

        fn bootstrap_object(
            &self,
            _instance: &SourceInstance,
            _object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            panic!("catalog registration must not bootstrap a source object")
        }

        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            panic!("catalog registration must not decode a source record")
        }
    }

    fn non_authorizing_engine(database_path: PathBuf) -> Arc<SpaghettiEngineCore> {
        let registry = AdapterRegistry::builder()
            .register(RegistrationOnlyAdapter::new("alpha"))
            .register(RegistrationOnlyAdapter::new("beta"))
            .register(RegistrationOnlyAdapter::new("gamma"))
            .build()
            .unwrap();
        SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path,
                query_workers: Some(1),
                owner_label: Some("catalog-build-non-authorizing-test".to_string()),
                defer_query_structures: false,
                source_pass_pool: None,
            },
            registry,
        )
        .unwrap()
    }

    fn root(directory: &TempDir, name: &str) -> PathBuf {
        let root = directory.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn empty_catalog_selection(query_pack_version: Option<u32>) -> ContractVersionSelection {
        ContractVersionSelection {
            selection_contract_version: crate::adapter::CONTRACT_VERSION_SELECTION_VERSION,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_version: 1,
            fact_family_versions: BTreeMap::from([
                ("catalog.project".to_string(), 1),
                ("catalog.session".to_string(), 1),
            ]),
            query_pack_version,
            observation_contract_version: None,
        }
    }

    #[test]
    fn split_registration_and_authority_preparation_do_not_read_catalog_sources() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("catalog.db");
        let engine = non_authorizing_engine(database_path.clone());
        let policy = CatalogAccessPolicyDigest::derive(1, b"withheld-test-policy").unwrap();

        let registered = engine
            .register_configured_catalog_sources(
                vec![CatalogConfiguredSource::new(
                    "alpha",
                    vec![root(&directory, "alpha")],
                    policy,
                )],
                QueryCancellationToken::default(),
            )
            .unwrap();
        let connection = Connection::open(&database_path).unwrap();
        let source_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        let object_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_objects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_count, 1, "configured source must be registered");
        assert_eq!(
            object_count, 0,
            "registration must not read catalog sources"
        );
        drop(connection);

        let preparation = engine
            .prepare_registered_catalog(
                registered,
                CatalogBuildIntent::Startup,
                QueryCancellationToken::default(),
            )
            .unwrap();
        assert!(matches!(
            preparation,
            CatalogBuildPreparation::AuthorizationUnavailable { adapter_ids }
                if adapter_ids == ["alpha"]
        ));
        assert!(engine.load_catalog_build_state().unwrap().is_none());

        let connection = Connection::open(&database_path).unwrap();
        let object_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_objects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            object_count, 0,
            "authority denial must precede catalog reads"
        );
        engine.shutdown().unwrap();
    }

    #[test]
    fn every_configured_source_registers_before_all_source_authority_denial() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("catalog.db");
        let engine = non_authorizing_engine(database_path.clone());
        let policy = CatalogAccessPolicyDigest::derive(1, b"withheld-test-policy").unwrap();

        let outcome = engine
            .reconcile_configured_catalog(
                vec![
                    CatalogConfiguredSource::new("gamma", vec![root(&directory, "gamma")], policy),
                    CatalogConfiguredSource::new("alpha", vec![root(&directory, "alpha")], policy),
                    CatalogConfiguredSource::new("beta", vec![root(&directory, "beta")], policy),
                ],
                CatalogBuildIntent::Startup,
                QueryCancellationToken::default(),
            )
            .unwrap();
        assert_eq!(
            outcome,
            CatalogBuildOutcome::AuthorizationUnavailable {
                adapter_ids: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string(),]
            }
        );
        assert!(engine.load_catalog_build_state().unwrap().is_none());

        let connection = Connection::open(database_path).unwrap();
        let source_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        let object_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_objects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_count, 3);
        assert_eq!(object_count, 0);
        engine.shutdown().unwrap();
    }

    #[test]
    fn duplicate_or_cancelled_catalog_configuration_mutates_nothing() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("catalog.db");
        let engine = non_authorizing_engine(database_path.clone());
        let policy = CatalogAccessPolicyDigest::derive(1, b"withheld-test-policy").unwrap();
        let source = CatalogConfiguredSource::new("alpha", vec![root(&directory, "alpha")], policy);

        assert!(matches!(
            engine.reconcile_configured_catalog(
                vec![source.clone(), source.clone()],
                CatalogBuildIntent::Startup,
                QueryCancellationToken::default(),
            ),
            Err(EngineError::InvalidConfig(_))
        ));
        let cancellation = QueryCancellationToken::default();
        cancellation.cancel();
        assert!(matches!(
            engine.reconcile_configured_catalog(
                vec![source],
                CatalogBuildIntent::Startup,
                cancellation,
            ),
            Err(EngineError::QueryCancelled)
        ));

        let connection = Connection::open(database_path).unwrap();
        let source_count: u64 = connection
            .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(source_count, 0);
        engine.shutdown().unwrap();
    }

    #[test]
    fn composition_failures_classify_source_loss_without_leaking_integrity_detail() {
        let source = catalog_composition_error(
            "produce catalog source",
            CatalogCompositionError::source_unavailable(
                "catalog producer failed to read a declared source object",
            ),
        );
        assert!(matches!(
            source,
            EngineError::Observation {
                operation: "produce catalog source",
                ..
            }
        ));

        let integrity = catalog_composition_error(
            "assemble catalog refresh projection",
            CatalogCompositionError::invalid("/Users/alice/private/catalog.json"),
        );
        assert!(matches!(
            integrity,
            EngineError::CatalogIntegrity {
                operation: "assemble catalog refresh projection"
            }
        ));
        let message = integrity.to_string();
        assert!(!message.contains("/Users/"));
        assert!(!message.contains("alice"));
        assert!(!message.contains("private"));
    }

    #[test]
    fn initial_publication_integrity_failure_is_durable_and_retried() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("catalog-initial-integrity.db");
        let engine = non_authorizing_engine(database_path);
        let plan = CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new())
            .unwrap();
        let invalid = PreparedCatalogBuild {
            plan: plan.clone(),
            selection: empty_catalog_selection(None),
            sources: Vec::new(),
            intent: CatalogBuildIntent::Startup,
        };
        assert!(matches!(
            engine.publish_prepared_catalog(invalid, QueryCancellationToken::default()),
            Err(EngineError::CatalogIntegrity {
                operation: "validate initial catalog publication"
            })
        ));
        let failed = engine.load_catalog_build_state().unwrap().unwrap();
        assert_eq!(failed.readiness.state, CatalogReadinessPhase::Error);
        assert_eq!(failed.readiness.attempt, 1);
        assert_eq!(failed.readiness.last_complete_snapshot, None);

        let valid = PreparedCatalogBuild {
            plan,
            selection: empty_catalog_selection(Some(
                crate::catalog_contract::CATALOG_QUERY_PACK_CONTRACT_VERSION,
            )),
            sources: Vec::new(),
            intent: CatalogBuildIntent::Startup,
        };
        let outcome = engine
            .publish_prepared_catalog(valid, QueryCancellationToken::default())
            .unwrap();
        assert!(matches!(
            outcome,
            CatalogBuildOutcome::Published {
                kind: CatalogPublicationKind::Initial,
                ..
            }
        ));
        let ready = engine.load_catalog_build_state().unwrap().unwrap();
        assert_eq!(ready.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(ready.readiness.attempt, 2);
        engine.shutdown().unwrap();
    }
}
