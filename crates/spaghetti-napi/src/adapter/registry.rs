use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use super::{
    AdapterError, AdapterId, AdapterSupportRegistration, AgentAdapter, CompatibilityDecision,
    ContractVersionOffer, ContractVersionRequest, NativeArtifactProbe, SupportCatalog,
    SupportOperation, TypedAccessAuthorization,
};

pub struct AdapterRegistryBuilder {
    adapters: Vec<Arc<dyn AgentAdapter>>,
}

impl AdapterRegistryBuilder {
    pub fn new() -> Self {
        Self {
            adapters: Vec::new(),
        }
    }

    pub fn register<A>(mut self, adapter: A) -> Self
    where
        A: AgentAdapter,
    {
        self.adapters.push(Arc::new(adapter));
        self
    }

    pub fn build(self) -> Result<AdapterRegistry, AdapterError> {
        self.build_inner(None)
    }

    /// Explicit compatibility path for hosts that predate promoted RFC 012A
    /// support packages. This registry cannot mint typed-access authority.
    pub fn build_legacy(self) -> Result<AdapterRegistry, AdapterError> {
        self.build_inner(None)
    }

    pub fn build_supported(
        self,
        support_catalog: Arc<SupportCatalog>,
    ) -> Result<AdapterRegistry, AdapterError> {
        self.build_inner(Some(support_catalog))
    }

    fn build_inner(
        self,
        support_catalog: Option<Arc<SupportCatalog>>,
    ) -> Result<AdapterRegistry, AdapterError> {
        let mut adapters = BTreeMap::new();
        for adapter in self.adapters {
            let manifest =
                catch_unwind(AssertUnwindSafe(|| adapter.manifest().clone())).map_err(|_| {
                    AdapterError::new(
                        super::AdapterErrorClass::AdapterFatal,
                        "adapter_panic",
                        "adapter panicked while declaring its manifest",
                    )
                })?;
            manifest.validate()?;
            if let Some(catalog) = &support_catalog {
                let binding = manifest.support_binding.as_ref().ok_or_else(|| {
                    AdapterError::invalid_contract(format!(
                        "adapter {} has no digest-bound support manifest",
                        manifest.id
                    ))
                })?;
                let scope_programs = manifest.scope_programs.as_ref().ok_or_else(|| {
                    AdapterError::invalid_contract(format!(
                        "adapter {} has no compiled scope declaration",
                        manifest.id
                    ))
                })?;
                catalog
                    .verify_adapter_registration(
                        AdapterSupportRegistration::new(
                            manifest.id.as_str(),
                            binding,
                            scope_programs,
                        ),
                        true,
                    )
                    .map_err(|error| AdapterError::invalid_contract(error.to_string()))?;
            }
            let id = manifest.id;
            if adapters.insert(id.clone(), adapter).is_some() {
                return Err(AdapterError::invalid_contract(format!(
                    "duplicate adapter id {id}"
                )));
            }
        }
        Ok(AdapterRegistry {
            adapters,
            support_catalog,
        })
    }
}

impl Default for AdapterRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AdapterRegistry {
    adapters: BTreeMap<AdapterId, Arc<dyn AgentAdapter>>,
    support_catalog: Option<Arc<SupportCatalog>>,
}

impl AdapterRegistry {
    pub fn builder() -> AdapterRegistryBuilder {
        AdapterRegistryBuilder::new()
    }

    pub fn get(&self, id: &AdapterId) -> Option<&Arc<dyn AgentAdapter>> {
        self.adapters.get(id)
    }

    pub fn resolve(&self, id: &str) -> Result<Arc<dyn AgentAdapter>, AdapterError> {
        let id = AdapterId::new(id)?;
        self.get(&id).cloned().ok_or_else(|| {
            AdapterError::invalid_contract(format!("adapter {id} is not registered"))
        })
    }

    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    pub fn enforces_promoted_support(&self) -> bool {
        self.support_catalog.is_some()
    }

    pub fn authorize_typed_access(
        &self,
        adapter_id: &AdapterId,
        probe: &NativeArtifactProbe,
        operation: SupportOperation,
        request: &ContractVersionRequest,
        offer: &ContractVersionOffer,
    ) -> Result<(CompatibilityDecision, TypedAccessAuthorization), AdapterError> {
        let adapter = self.get(adapter_id).ok_or_else(|| {
            AdapterError::invalid_contract(format!("adapter {adapter_id} is not registered"))
        })?;
        let binding = adapter.manifest().support_binding.as_ref().ok_or_else(|| {
            AdapterError::invalid_contract(format!(
                "adapter {adapter_id} has no digest-bound support manifest"
            ))
        })?;
        let scope_programs = adapter.manifest().scope_programs.as_ref().ok_or_else(|| {
            AdapterError::invalid_contract(format!(
                "adapter {adapter_id} has no compiled scope declaration"
            ))
        })?;
        let catalog = self.support_catalog.as_ref().ok_or_else(|| {
            AdapterError::invalid_contract(
                "adapter registry was built through the explicit legacy compatibility path",
            )
        })?;
        catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new(adapter_id.as_str(), binding, scope_programs),
                probe,
                operation,
                request,
                offer,
            )
            .map_err(|error| AdapterError::invalid_contract(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::adapter::{
        verify_support_release_bundle, AdapterManifest, AdapterObjectContext,
        AdapterSupportBinding, CoverageAbsenceKind, CoverageDomain, CoveragePositionKind,
        CoverageSetCompleteness, CoverageStatus, DecodeContext, DecodeDisposition, DecoderId,
        DiscoveryContext, ExternalEntityRef, Fact, FactBatch, FactSemanticContext,
        RawRetentionPolicy, Sha256Digest, SourceAccess, SourceInstance, SourceInstanceSpec,
        SourceObjectDescriptor, StreamSpec, SupportBundleDocument,
    };
    use crate::scoped_observation::{
        ScopedActorAttribution, ScopedActorFallbackReason, ScopedAdmissionError,
        ScopedAppendDecodeOutcome, ScopedAppendDecoderConfig, ScopedAppendDeliveryPhase,
        ScopedAppendObservation, ScopedAppendPresenceChange, ScopedAppendReconcileRequest,
        ScopedBootstrapBarrierError, ScopedContinuityError, ScopedCoverageAssemblyError,
        ScopedDecodeFailureClass, ScopedDecodedAppendItem, ScopedDeliveryError,
        ScopedEnvelopeEvidenceAuthority, ScopedKnownAppendObject, ScopedKnownObjectGrant,
        ScopedKnownObjectReadRequest, ScopedObjectRead, ScopedObservationAccessError,
        ScopedObservationAccessHost, ScopedObservationAccessPass, ScopedObservationAccessRequest,
        ScopedObservationAdmissionLane, ScopedObservationAsyncRuntime, ScopedObservationCloseError,
        ScopedObservationConsumerOfferError, ScopedObservationContinuity,
        ScopedObservationDeliveryLane, ScopedObservationDeliveryLimits, ScopedObservationEvent,
        ScopedObservationNativeWatchBackend, ScopedObservationNativeWatcherError,
        ScopedObservationOpenDrainError, ScopedObservationPollError, ScopedObservationPollLease,
        ScopedObservationPollResolution, ScopedObservationProjectionLimits,
        ScopedObservationProjectionSink, ScopedObservationQueueLimits,
        ScopedObservationReadyResolution, ScopedObservationStartupError,
        ScopedObservationStartupReconcileAction, ScopedObservationWatcherHintAction,
        ScopedObservationWatcherPhase, ScopedProjectionDeliveryError, ScopedQueuedObservationFrame,
        ScopedReplacementMode, ScopedReplacementRepresentation, ScopedReplacementStageError,
        ScopedResyncReason, ScopedRootIdentityRequest, ScopedSourceFailureClass,
    };
    use crate::source::{
        AccessOperation, AccessOutcome, AccessPhase, AppendDelimitedConfig, AppendDelimitedFile,
        AppendItem, AppendRead, AppendTransition, AuthorizedScopeAccessPlan, DirtyHint,
        DirtyReason, DirtyScope, HintEnqueue, RecordOrigin, ScopeAccessReport, ScopeAccessRequest,
        ScopeIdentityInput, SourceMediaType, SourceRecord,
    };

    use super::*;

    struct ImmediateScopedWatchBackend {
        callback: Box<dyn FnMut(notify::Result<notify::Event>) + Send + 'static>,
        target: PathBuf,
        unrelated: PathBuf,
        registrations: Arc<std::sync::Mutex<Vec<(PathBuf, notify::RecursiveMode)>>>,
        fail_registration: bool,
    }

    impl ScopedObservationNativeWatchBackend for ImmediateScopedWatchBackend {
        fn watch(&mut self, path: &std::path::Path, mode: notify::RecursiveMode) -> Result<(), ()> {
            self.registrations
                .lock()
                .unwrap()
                .push((path.to_path_buf(), mode));
            if self.fail_registration {
                return Err(());
            }
            (self.callback)(Ok(notify::Event::new(notify::EventKind::Access(
                notify::event::AccessKind::Any,
            ))
            .add_path(self.target.clone())));
            (self.callback)(Ok(notify::Event::new(notify::EventKind::Modify(
                notify::event::ModifyKind::Any,
            ))
            .add_path(self.unrelated.clone())));
            (self.callback)(Ok(notify::Event::new(notify::EventKind::Create(
                notify::event::CreateKind::File,
            ))
            .add_path(self.target.clone())));
            Ok(())
        }
    }

    struct EmptyAdapter {
        manifest: AdapterManifest,
        decode_statefully: bool,
        request_dependency_access: bool,
    }

    impl EmptyAdapter {
        fn new(id: &str) -> Self {
            Self {
                manifest: AdapterManifest {
                    id: AdapterId::new(id).unwrap(),
                    display_name: id.to_string(),
                    adapter_version: "1.0.0".to_string(),
                    contract_version: 1,
                    support_binding: None,
                    scope_programs: None,
                    source_schema_versions: Vec::new(),
                    capabilities: Vec::new(),
                },
                decode_statefully: false,
                request_dependency_access: false,
            }
        }

        fn with_stateful_decode(mut self) -> Self {
            self.decode_statefully = true;
            self
        }

        fn with_dependency_access(mut self) -> Self {
            self.request_dependency_access = true;
            self
        }

        fn with_support(
            mut self,
            binding: AdapterSupportBinding,
            scope_programs: crate::adapter::ScopeProgramManifest,
        ) -> Self {
            self.manifest.support_binding = Some(binding);
            self.manifest.scope_programs = Some(scope_programs);
            self
        }
    }

    fn promoted_fixture_catalog() -> (
        Arc<SupportCatalog>,
        AdapterSupportBinding,
        crate::adapter::ScopeProgramManifest,
    ) {
        let documents = [
            (
                "ads",
                "support/ads.json",
                br#"{"adapter_id":"fixture","ads_id":"fixture-ads"}"#.as_slice(),
            ),
            (
                "source_declaration",
                "support/source.json",
                br#"{"adapter_id":"fixture","ads_id":"fixture-ads"}"#.as_slice(),
            ),
            (
                "scope_program",
                "support/scope.json",
                br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#.as_slice(),
            ),
            (
                "evidence",
                "support/evidence.json",
                br#"{"adapter_id":"fixture","ads_id":"fixture-ads"}"#.as_slice(),
            ),
            (
                "conformance",
                "support/conformance.json",
                br#"{"adapter_id":"fixture","support_release_id":"fixture-release"}"#.as_slice(),
            ),
        ];
        let references = documents
            .iter()
            .map(|(kind, path, bytes)| {
                (
                    (*kind).to_string(),
                    serde_json::json!({"path": path, "sha256": Sha256Digest::of(bytes).to_string()}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let release_json = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "support_release_id": "fixture-release",
            "adapter_id": "fixture",
            "status": "promoted",
            "artifact_compatibility": {
                "family": "fixture",
                "platforms": ["test"],
                "exact_versions": ["1.0.0"],
                "ranges": [],
                "required_markers": ["fixture.marker"],
                "forward_catalog_only": false
            },
            "references": references,
            "versions": {"adapter_package": "1.0.0", "decoder_contract": 1}
        }))
        .unwrap();
        let bundle_documents = documents
            .iter()
            .map(|(_, path, bytes)| SupportBundleDocument::new(path, bytes))
            .collect::<Vec<_>>();
        let release = verify_support_release_bundle(&release_json, &bundle_documents).unwrap();
        let binding = release.adapter_binding().clone();
        let scope_programs = release.scope_programs().clone();
        (
            Arc::new(SupportCatalog::new([release]).unwrap()),
            binding,
            scope_programs,
        )
    }

    fn supported_fixture_registry() -> AdapterRegistry {
        let (catalog, binding, scope_programs) = promoted_fixture_catalog();
        AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("fixture").with_support(binding, scope_programs))
            .build_supported(catalog)
            .unwrap()
    }

    fn stateful_supported_fixture_registry() -> AdapterRegistry {
        let (catalog, binding, scope_programs) = promoted_fixture_catalog();
        AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_stateful_decode(),
            )
            .build_supported(catalog)
            .unwrap()
    }

    fn dependency_supported_fixture_registry() -> AdapterRegistry {
        let (catalog, binding, scope_programs) = promoted_fixture_catalog();
        AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_stateful_decode()
                    .with_dependency_access(),
            )
            .build_supported(catalog)
            .unwrap()
    }

    fn scoped_access_request(root: PathBuf) -> ScopedObservationAccessRequest {
        ScopedObservationAccessRequest {
            adapter_id: "fixture".to_string(),
            artifact_probe: NativeArtifactProbe {
                family: "fixture".to_string(),
                platform: "test".to_string(),
                version: Some("1.0.0".to_string()),
                markers: vec!["fixture.marker".to_string()],
                contradictory_markers: false,
            },
            contract_request: ContractVersionRequest {
                selection_contract_version: 1,
                model_major: 1,
                external_entity_reference_version: 1,
                semantic_revision_reference_version: 1,
                coverage_contract_versions: vec![1],
                fact_family_versions: BTreeMap::from([("runtime.usage-v2".to_string(), vec![1])]),
                query_pack_versions: None,
                observation_contract_versions: Some(vec![1]),
            },
            contract_offer: ContractVersionOffer {
                selection_contract_version: 1,
                model_major: 1,
                external_entity_reference_versions: vec![1],
                semantic_revision_reference_versions: vec![1],
                coverage_contract_versions: vec![1],
                fact_family_versions: BTreeMap::from([("runtime.usage-v2".to_string(), vec![1])]),
                query_pack_versions: Vec::new(),
                observation_contract_versions: vec![1],
            },
            root_identity: ScopedRootIdentityRequest::new(
                1,
                b"fixture-source-instance".as_slice(),
                b"fixture-session".as_slice(),
                None,
                None,
                None,
            ),
            program_id: "observe-session".to_string(),
            known_objects: vec![ScopedKnownObjectGrant {
                relation_id: "root-object".to_string(),
                scope_root: true,
                access_root: "root".to_string(),
                locator_id: "known-object".to_string(),
                root,
                relative_path: "session.jsonl".into(),
            }],
        }
    }

    fn decode_scoped(
        host: &ScopedObservationAccessHost,
        object: &mut ScopedKnownAppendObject,
        observation: &ScopedAppendObservation,
    ) -> ScopedAppendDecodeOutcome {
        host.decode_append(object, observation).unwrap()
    }

    fn scoped_append_object(
        driver: AppendDelimitedFile,
        retention: RawRetentionPolicy,
    ) -> ScopedKnownAppendObject {
        scoped_append_object_with_coverage(driver, retention, Vec::new())
    }

    fn scoped_append_object_with_coverage(
        driver: AppendDelimitedFile,
        retention: RawRetentionPolicy,
        coverage_domains: Vec<CoverageDomain>,
    ) -> ScopedKnownAppendObject {
        ScopedKnownAppendObject::new(
            driver,
            ScopedAppendDecoderConfig {
                decoder: DecoderId::new("fixture-decoder").unwrap(),
                object_context: AdapterObjectContext::empty(),
                semantic_context: fixture_semantic_context(),
                coverage_domains,
                retention,
                max_facts_per_record: 16,
                max_diagnostics_per_record: 16,
            },
        )
        .unwrap()
    }

    fn fixture_semantic_context() -> FactSemanticContext {
        fixture_semantic_context_for_source(b"fixture-source-instance")
    }

    fn fixture_semantic_context_for_source(
        stable_source_instance_discriminator: &[u8],
    ) -> FactSemanticContext {
        FactSemanticContext::new(
            &AdapterId::new("fixture").unwrap(),
            1,
            stable_source_instance_discriminator,
            b"fixture-transcript",
            b"session.jsonl",
            1,
        )
        .unwrap()
    }

    fn admission_lane(
        max_data_events: u64,
        max_retained_native_bytes: u64,
        max_control_items: usize,
    ) -> ScopedObservationAdmissionLane {
        ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events,
            max_retained_native_bytes,
            max_control_items,
            max_coverage_objects: 1,
        })
        .unwrap()
    }

    fn decode_and_admit_ignored(
        host: &ScopedObservationAccessHost,
        object: &mut ScopedKnownAppendObject,
        observation: &ScopedAppendObservation,
    ) {
        let decoded = decode_scoped(host, object, observation);
        let ScopedAppendDecodeOutcome::Ready(batch) = decoded else {
            panic!("fixture decoder must not request a retry");
        };
        let mut lane = admission_lane(64, 64 * 1024, 4);
        if let Err(failure) = lane.admit(object, observation, batch) {
            panic!("fixture admission failed: {}", failure.error);
        }
    }

    fn reconcile_missing_poll(
        host: &ScopedObservationAccessHost,
        lease: &ScopedObservationPollLease,
        object: &mut ScopedKnownAppendObject,
        admission: &mut ScopedObservationAdmissionLane,
        identity_inputs: &[ScopeIdentityInput<'_>],
        origin: &RecordOrigin,
        access_phase: AccessPhase,
    ) {
        let observation = object
            .reconcile(
                lease.access_pass(),
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs,
                    access_phase,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();
        assert!(!observation.object_present);
        let ScopedAppendDecodeOutcome::Ready(decoded) = decode_scoped(host, object, &observation)
        else {
            panic!("stable missing source must produce a complete poll observation");
        };
        if let Err(failure) = admission.admit(object, &observation, decoded) {
            panic!("missing-source poll admission failed: {}", failure.error);
        }
        assert!(admission.is_empty());
    }

    fn reconcile_scoped_append(
        host: &ScopedObservationAccessHost,
        pass: &ScopedObservationAccessPass,
        object: &mut ScopedKnownAppendObject,
        admission: &mut ScopedObservationAdmissionLane,
        identity_inputs: &[ScopeIdentityInput<'_>],
        origin: &RecordOrigin,
        access_phase: AccessPhase,
    ) -> ScopedAppendObservation {
        let observation = object
            .reconcile(
                pass,
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs,
                    access_phase,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();
        let ScopedAppendDecodeOutcome::Ready(decoded) = decode_scoped(host, object, &observation)
        else {
            panic!("startup fixture must produce a complete decoded observation");
        };
        if let Err(failure) = admission.admit(object, &observation, decoded) {
            panic!("startup fixture admission failed: {}", failure.error);
        }
        observation
    }

    impl AgentAdapter for EmptyAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            _context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            Ok(Vec::new())
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(Vec::new())
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
            context: DecodeContext<'_>,
            record: &SourceRecord,
            output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            if !self.decode_statefully {
                return Ok(DecodeDisposition::IgnoredKnown);
            }
            if record.payload == b"retry" {
                return Ok(DecodeDisposition::RetryTransient);
            }
            output.push_derived(
                record,
                b"fixture-unknown-record",
                Fact::UnknownRecord {
                    native_kind: Some("fixture".to_string()),
                    raw_payload: record.payload.clone(),
                    reason: "fixture".to_string(),
                },
            )?;
            let mut state = context.decoder_state.unwrap_or_default().to_vec();
            state.extend_from_slice(&record.payload);
            output.set_next_decoder_state(state)?;
            Ok(DecodeDisposition::PreservedUnknown)
        }

        fn decode_with_access(
            &self,
            context: DecodeContext<'_>,
            record: &SourceRecord,
            output: &mut FactBatch,
            source_access: &dyn SourceAccess,
        ) -> Result<DecodeDisposition, AdapterError> {
            if self.request_dependency_access {
                source_access.read_object("root", std::path::Path::new("sidecar.json"), 16)?;
            }
            self.decode(context, record, output)
        }
    }

    #[test]
    fn registry_rejects_duplicate_open_ids() {
        let result = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("same"))
            .register(EmptyAdapter::new("same"))
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn registry_resolves_an_adapter_without_source_specific_dispatch() {
        let registry = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("one"))
            .register(EmptyAdapter::new("two"))
            .build()
            .unwrap();
        assert_eq!(registry.len(), 2);
        assert!(registry.get(&AdapterId::new("two").unwrap()).is_some());
    }

    #[test]
    fn supported_registry_requires_and_retains_a_promoted_digest_binding() {
        let (catalog, binding, scope_programs) = promoted_fixture_catalog();
        let missing = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("fixture"))
            .build_supported(Arc::clone(&catalog));
        assert!(missing.err().unwrap().to_string().contains("digest-bound"));

        let mut mismatched_scope = scope_programs.clone();
        mismatched_scope.programs[0].relations[0].locator = "different-object".to_string();
        let mismatched = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("fixture").with_support(binding.clone(), mismatched_scope))
            .build_supported(Arc::clone(&catalog));
        assert!(mismatched
            .err()
            .unwrap()
            .to_string()
            .contains("compiled scope declarations"));

        let registry = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("fixture").with_support(binding, scope_programs))
            .build_supported(catalog)
            .unwrap();
        assert!(registry.enforces_promoted_support());

        let request = ContractVersionRequest {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: BTreeMap::new(),
            query_pack_versions: Some(vec![1]),
            observation_contract_versions: None,
        };
        let offer = ContractVersionOffer {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: BTreeMap::new(),
            query_pack_versions: vec![1],
            observation_contract_versions: Vec::new(),
        };
        let (decision, authorization) = registry
            .authorize_typed_access(
                &AdapterId::new("fixture").unwrap(),
                &NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                },
                SupportOperation::DurableHistoryRuntime,
                &request,
                &offer,
            )
            .unwrap();
        assert_eq!(decision.support_release_id(), Some("fixture-release"));
        assert_eq!(
            authorization.operation().support_release_id(),
            Some("fixture-release")
        );
        assert!(authorization
            .select_scope_program("observe-session")
            .is_err());

        let missing_observation_contract = registry.authorize_typed_access(
            &AdapterId::new("fixture").unwrap(),
            &NativeArtifactProbe {
                family: "fixture".to_string(),
                platform: "test".to_string(),
                version: Some("1.0.0".to_string()),
                markers: vec!["fixture.marker".to_string()],
                contradictory_markers: false,
            },
            SupportOperation::ScopedTypedObservation,
            &request,
            &offer,
        );
        assert!(missing_observation_contract
            .unwrap_err()
            .to_string()
            .contains("observation contract"));

        let mut scoped_request = request;
        scoped_request.observation_contract_versions = Some(vec![1]);
        let mut scoped_offer = offer;
        scoped_offer.observation_contract_versions = vec![1];
        let (_, scoped_authorization) = registry
            .authorize_typed_access(
                &AdapterId::new("fixture").unwrap(),
                &NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                },
                SupportOperation::ScopedTypedObservation,
                &scoped_request,
                &scoped_offer,
            )
            .unwrap();
        let program = scoped_authorization
            .select_scope_program("observe-session")
            .unwrap();
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program).unwrap();
        assert_eq!(plan.adapter_id(), "fixture");
        assert_eq!(plan.support_release_id(), "fixture-release");

        let empty_report = plan.report();
        assert!(empty_report.verify_digest());
        let empty_digest = empty_report.digest();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"secret-session-id",
        }];
        plan.reserve(ScopeAccessRequest {
            relation_id: "root-object",
            operation: AccessOperation::ObjectRead,
            phase: AccessPhase::Initial,
            parent_token: None,
            identity_inputs: &identity,
            depth: 1,
            max_bytes: 128,
            max_rows: 0,
        })
        .unwrap()
        .complete(64, 0, AccessOutcome::Available)
        .unwrap();
        let report = plan.report();
        assert!(report.verify_digest());
        assert_eq!(
            report.digest().to_string(),
            "sha256:3d061fe88fd820c3bebf534b13276ee0b4258f957da006dfa0628ddd97924e52"
        );
        assert_ne!(report.digest(), empty_digest);
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("secret-session-id"));

        let report_value = serde_json::to_value(report).unwrap();
        let mut unknown_field = report_value.clone();
        unknown_field["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ScopeAccessReport>(unknown_field).is_err());

        let mut tampered = report_value;
        tampered["relations"][0]["bytes_read"] = serde_json::json!(65);
        let tampered: ScopeAccessReport = serde_json::from_value(tampered).unwrap();
        assert!(!tampered.verify_digest());
    }

    #[test]
    fn scoped_host_owns_authorization_confined_access_and_report_lifecycle() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("authorized-root");
        std::fs::create_dir_all(&root).unwrap();
        let request = scoped_access_request(root.clone());

        let dropped_host_request = request.clone();
        let mut escaped_request = request.clone();
        escaped_request.known_objects[0].relative_path = "../outside.jsonl".into();
        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, escaped_request),
            Err(ScopedObservationAccessError::InvalidGrant(_))
        ));

        // Attachment authorizes one exact missing object without reading it;
        // the native process may create the root transcript afterward.
        let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
        assert_eq!(
            host.compatibility().support_release_id(),
            Some("fixture-release")
        );
        assert!(matches!(
            host.open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 0,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            }),
            Err(ScopedObservationOpenDrainError::Delivery(
                ScopedDeliveryError::InvalidLimits
            ))
        ));
        let drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        assert_eq!(drain.delivery_lane().state().offered_through_sequence, 0);
        assert_eq!(drain.state().applied_through_sequence, 0);
        assert!(matches!(
            host.open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            }),
            Err(ScopedObservationOpenDrainError::AlreadyOpened)
        ));
        drop(drain);
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"secret-session-id",
        }];
        let missing_pass = host.begin_pass().unwrap();
        assert_eq!(
            missing_pass
                .read_known_object(ScopedKnownObjectReadRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    phase: AccessPhase::Initial,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 128,
                })
                .unwrap(),
            ScopedObjectRead::Unavailable
        );
        let missing_report = missing_pass.finish();
        assert_eq!(
            missing_report.relations()[0].trace[0].outcome,
            AccessOutcome::Unavailable
        );

        std::fs::write(root.join("session.jsonl"), b"native transcript bytes\n").unwrap();
        let pass = host.begin_pass().unwrap();
        assert!(matches!(
            host.begin_pass(),
            Err(ScopedObservationAccessError::PassAlreadyActive)
        ));
        let read = pass
            .read_known_object(ScopedKnownObjectReadRequest {
                relation_id: "root-object",
                identity_inputs: &identity,
                phase: AccessPhase::Initial,
                parent_token: None,
                depth: 1,
                max_bytes: 128,
            })
            .unwrap();
        assert!(matches!(
            read,
            ScopedObjectRead::Available { ref bytes, .. }
                if bytes == b"native transcript bytes\n"
        ));
        let report = pass.finish();
        assert!(report.verify_digest());
        assert_eq!(report.relations()[0].attempts, 1);
        assert_eq!(report.relations()[0].bytes_read, 24);
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("secret-session-id"));
        assert!(!encoded.contains("session.jsonl"));
        assert!(!encoded.contains("native transcript bytes"));

        let next_pass = host.begin_pass().unwrap();
        assert_eq!(next_pass.report().relations()[0].attempts, 0);
        drop(next_pass);
        host.close();
        host.close();
        assert!(host.is_closed());
        assert!(matches!(
            host.begin_pass(),
            Err(ScopedObservationAccessError::Closed)
        ));
        assert!(matches!(
            host.open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            }),
            Err(ScopedObservationOpenDrainError::Closed)
        ));

        let dropped_host =
            ScopedObservationAccessHost::authorize(&registry, dropped_host_request).unwrap();
        let orphaned_pass = dropped_host.begin_pass().unwrap();
        drop(dropped_host);
        assert!(matches!(
            orphaned_pass.read_known_object(ScopedKnownObjectReadRequest {
                relation_id: "root-object",
                identity_inputs: &identity,
                phase: AccessPhase::Initial,
                parent_token: None,
                depth: 1,
                max_bytes: 128,
            }),
            Err(ScopedObservationAccessError::Closed)
        ));
    }

    #[tokio::test]
    async fn scoped_poll_coalesces_prepass_requests_and_defers_inflight_requests() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("poll-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let mut drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 2,
            })
            .unwrap();
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let mut admission = admission_lane(1, 0, 1);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"poll-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };

        let first = host.request_poll().unwrap();
        let second = host.request_poll().unwrap();
        assert_eq!(first.request_generation(), 1);
        assert_eq!(second.request_generation(), 2);
        let waiting_second = second.clone();
        let async_poll = tokio::spawn(async move { waiting_second.wait_async().await });
        tokio::task::yield_now().await;
        let waiting_first = first.clone();
        let (poll_tx, poll_rx) = std::sync::mpsc::sync_channel(0);
        let poll_thread = std::thread::spawn(move || {
            poll_tx.send(waiting_first.wait()).unwrap();
        });
        let incomplete = host.begin_poll().unwrap().unwrap();
        assert_eq!(incomplete.target_generation(), 2);
        assert!(matches!(
            host.complete_bootstrap_poll(incomplete, &admission, &projection, &drain),
            Err(ScopedObservationPollError::IncompleteScopePass)
        ));
        assert_eq!(
            host.poll_resolution(&first).unwrap(),
            ScopedObservationPollResolution::Pending
        );

        // The failed completion requeues the same target. A new request after
        // reservation is conservatively left for a follow-up pass.
        let lease = host.begin_poll().unwrap().unwrap();
        assert_eq!(lease.target_generation(), 2);
        assert!(host.begin_poll().unwrap().is_none());
        let third = host.request_poll().unwrap();
        assert_eq!(third.request_generation(), 3);
        reconcile_missing_poll(
            &host,
            &lease,
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Initial,
        );
        let first_watermark = host
            .complete_bootstrap_poll(lease, &admission, &projection, &drain)
            .unwrap();
        assert_eq!(first_watermark.offered_through_sequence, 0);
        let first_result = host.poll_resolution(&first).unwrap();
        let second_result = host.poll_resolution(&second).unwrap();
        let (
            ScopedObservationPollResolution::Ready(first_result),
            ScopedObservationPollResolution::Ready(second_result),
        ) = (first_result, second_result)
        else {
            panic!("both pre-pass poll requests must share one completion");
        };
        assert!(Arc::ptr_eq(&first_result, &second_result));
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), async_poll)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationPollResolution::Ready(waited)
                if Arc::ptr_eq(&waited, &first_watermark)
        ));
        let waited = poll_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("offered poll completion must wake its request-local waiter");
        assert!(matches!(
            waited,
            ScopedObservationPollResolution::Ready(waited)
                if Arc::ptr_eq(&waited, &first_watermark)
        ));
        poll_thread.join().unwrap();
        assert_eq!(
            host.poll_resolution(&third).unwrap(),
            ScopedObservationPollResolution::Pending
        );

        let follow_up = host.begin_poll().unwrap().unwrap();
        assert_eq!(follow_up.target_generation(), 3);
        reconcile_missing_poll(
            &host,
            &follow_up,
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Revalidation,
        );
        let follow_up_watermark = host
            .complete_bootstrap_poll(follow_up, &admission, &projection, &drain)
            .unwrap();
        assert_eq!(follow_up_watermark.offered_through_sequence, 0);
        let ScopedObservationPollResolution::Ready(retained_first_watermark) =
            host.poll_resolution(&first).unwrap()
        else {
            panic!("a completed ticket must retain its original poll result");
        };
        assert!(Arc::ptr_eq(&retained_first_watermark, &first_watermark));
        assert!(!Arc::ptr_eq(&first_watermark, &follow_up_watermark));
        assert!(matches!(
            host.poll_resolution(&third).unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));

        // A raw access attempt cannot satisfy poll completion using coverage
        // left by an older pass; decode/admission/offered promotion must carry
        // the current pass identity all the way to the watermark.
        let read_only = host.request_poll().unwrap();
        let read_only_lease = host.begin_poll().unwrap().unwrap();
        assert_eq!(
            read_only_lease
                .access_pass()
                .read_known_object(ScopedKnownObjectReadRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    phase: AccessPhase::Revalidation,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                })
                .unwrap(),
            ScopedObjectRead::Unavailable
        );
        assert!(matches!(
            host.complete_bootstrap_poll(read_only_lease, &admission, &projection, &drain,),
            Err(ScopedObservationPollError::IncompleteScopePass)
        ));
        assert_eq!(
            host.poll_resolution(&read_only).unwrap(),
            ScopedObservationPollResolution::Pending
        );
        let retry = host.begin_poll().unwrap().unwrap();
        reconcile_missing_poll(
            &host,
            &retry,
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Revalidation,
        );
        host.complete_bootstrap_poll(retry, &admission, &projection, &drain)
            .unwrap();
        assert!(matches!(
            host.poll_resolution(&read_only).unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));
        assert_eq!(
            host.poll_state(),
            crate::scoped_observation::ScopedObservationPollState {
                requested_through_generation: 4,
                completed_through_generation: 4,
                in_flight_through_generation: None,
                closed: false,
            }
        );

        object.complete_bootstrap().unwrap();
        let async_ready = host.ready_waiter().unwrap();
        let ready_task = tokio::spawn(async move { async_ready.wait_async().await });
        tokio::task::yield_now().await;
        let barrier = host
            .offer_consumer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut drain,
                50,
            )
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), ready_task)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationReadyResolution::Ready(waited)
                if Arc::ptr_eq(&waited, &barrier)
        ));
        assert!(Arc::ptr_eq(
            &host.engine_ready(&drain).unwrap().unwrap(),
            &barrier
        ));
        let mut active = host
            .bind_consumer_bootstrap_epoch_state(vec![object], admission, projection, &drain)
            .unwrap();

        // An unchanged live pass may advance/confirm offered coverage but must
        // not manufacture another semantic or lifecycle event.
        let before = drain.delivery_lane().state();
        let unchanged = host.request_poll().unwrap();
        let lease = host.begin_poll().unwrap().unwrap();
        let (object, admission) = active
            .append_object_and_admission_mut("root-object")
            .unwrap();
        reconcile_missing_poll(
            &host,
            &lease,
            object,
            admission,
            &identity,
            &origin,
            AccessPhase::Revalidation,
        );
        let unchanged_watermark = host.complete_epoch_poll(lease, &active, &drain).unwrap();
        assert_eq!(
            unchanged_watermark.offered_through_sequence,
            before.offered_through_sequence
        );
        assert_eq!(drain.delivery_lane().state(), before);
        assert!(matches!(
            host.poll_resolution(&unchanged).unwrap(),
            ScopedObservationPollResolution::Ready(watermark)
                if watermark.offered_through_sequence == barrier.barrier_sequence
        ));
    }

    #[test]
    fn scoped_poll_rejects_cross_attachment_state_and_cancels_pending_on_close() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let first_root = temp.path().join("first-poll-root");
        let second_root = temp.path().join("second-poll-root");
        std::fs::create_dir_all(&first_root).unwrap();
        std::fs::create_dir_all(&second_root).unwrap();
        let first =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(first_root))
                .unwrap();
        let second =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(second_root))
                .unwrap();
        let limits = ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        };
        let first_drain = first.open_consumer_drain(limits).unwrap();
        let mut second_drain = second.open_consumer_drain(limits).unwrap();
        assert!(matches!(
            first.engine_ready(&second_drain),
            Err(ScopedObservationPollError::ForeignDrain)
        ));
        assert!(matches!(
            first.close_with_consumer(&mut second_drain),
            Err(ScopedObservationCloseError::ForeignDrain)
        ));
        assert!(!first.is_closed());
        assert!(first.engine_ready(&first_drain).unwrap().is_none());
        let cancelled_ready = first.ready_waiter().unwrap();

        let ticket = first.request_poll().unwrap();
        let cancelled_poll = ticket.clone();
        assert!(matches!(
            second.poll_resolution(&ticket),
            Err(ScopedObservationPollError::ForeignTicket)
        ));
        let lease = first.begin_poll().unwrap().unwrap();
        assert_eq!(first.poll_state().in_flight_through_generation, Some(1));
        first.close();
        assert_eq!(
            cancelled_poll.wait(),
            ScopedObservationPollResolution::Cancelled
        );
        assert_eq!(
            cancelled_ready.wait(),
            ScopedObservationReadyResolution::Cancelled
        );
        assert_eq!(
            first.poll_resolution(&ticket).unwrap(),
            ScopedObservationPollResolution::Cancelled
        );
        assert!(matches!(
            first.request_poll(),
            Err(ScopedObservationPollError::Closed)
        ));
        assert!(matches!(
            first.begin_poll(),
            Err(ScopedObservationPollError::Closed)
        ));
        drop(lease);
        assert_eq!(first.poll_state().in_flight_through_generation, None);
        assert!(first.poll_state().closed);
    }

    #[test]
    fn scoped_close_barrier_waits_for_active_pass_and_consumer_acknowledgement() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("close-barrier-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let mut drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        let ticket = host.request_poll().unwrap();
        let lease = host.begin_poll().unwrap().unwrap();
        let barrier = host.close();
        assert_eq!(
            host.poll_resolution(&ticket).unwrap(),
            ScopedObservationPollResolution::Cancelled
        );
        let closing = barrier.state();
        assert!(closing.close_requested);
        assert_eq!(closing.active_operations, 1);
        assert_eq!(closing.active_watcher_tasks, 0);
        assert!(closing.consumer_drain_pending);
        assert!(!closing.complete);

        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"close-barrier-session",
        }];
        assert!(matches!(
            lease
                .access_pass()
                .read_known_object(ScopedKnownObjectReadRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    phase: AccessPhase::Revalidation,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                }),
            Err(ScopedObservationAccessError::Closed)
        ));

        drain.close();
        assert!(drain.is_closed());
        let drain_closed = barrier.state();
        assert_eq!(drain_closed.active_operations, 1);
        assert_eq!(drain_closed.active_watcher_tasks, 0);
        assert!(!drain_closed.consumer_drain_pending);
        assert!(!drain_closed.complete);
        drop(lease);

        let complete = barrier.wait();
        assert_eq!(complete.active_operations, 0);
        assert_eq!(complete.active_watcher_tasks, 0);
        assert!(!complete.consumer_drain_pending);
        assert!(complete.complete);
        let repeated = host.close_with_consumer(&mut drain).unwrap();
        assert!(repeated.state().complete);
        assert!(matches!(
            host.begin_pass(),
            Err(ScopedObservationAccessError::Closed)
        ));
    }

    #[test]
    fn scoped_close_cancels_and_waits_for_registered_watcher_tasks() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("watcher-close-root")),
        )
        .unwrap();
        let first = host.register_watcher_task().unwrap();
        let second = host.register_watcher_task().unwrap();
        assert!(!first.cancellation_requested());
        assert!(!second.cancellation_requested());

        let barrier = host.close();
        assert!(first.cancellation_requested());
        assert!(second.cancellation_requested());
        let closing = barrier.state();
        assert_eq!(closing.active_operations, 2);
        assert_eq!(closing.active_watcher_tasks, 2);
        assert!(!closing.consumer_drain_pending);
        assert!(!closing.complete);
        assert!(matches!(
            host.register_watcher_task(),
            Err(ScopedObservationAccessError::Closed)
        ));

        drop(first);
        let one_pending = barrier.state();
        assert_eq!(one_pending.active_operations, 1);
        assert_eq!(one_pending.active_watcher_tasks, 1);
        assert!(!one_pending.complete);

        drop(second);
        let complete = barrier.wait();
        assert_eq!(complete.active_operations, 0);
        assert_eq!(complete.active_watcher_tasks, 0);
        assert!(complete.complete);
    }

    #[test]
    fn dropping_scoped_host_requests_watcher_cancellation_without_blocking() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let registration = {
            let host = ScopedObservationAccessHost::authorize(
                &registry,
                scoped_access_request(temp.path().join("watcher-drop-root")),
            )
            .unwrap();
            let registration = host.register_watcher_task().unwrap();
            assert!(!registration.cancellation_requested());
            drop(host);
            registration
        };

        assert!(registration.cancellation_requested());
        drop(registration);
    }

    #[test]
    fn scoped_close_wakes_a_registered_watcher_before_waiting_for_acknowledgement() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("watcher-wake-root")),
        )
        .unwrap();
        let registration = host.register_watcher_task().unwrap();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::sync_channel(0);
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::sync_channel(0);
        let watcher = std::thread::spawn(move || {
            waiting_tx.send(()).unwrap();
            registration.wait_for_cancellation();
            cancelled_tx.send(()).unwrap();
            // Dropping registration after this send is the watcher's shutdown
            // acknowledgement to the attachment close barrier.
        });
        waiting_rx.recv().unwrap();

        let barrier = host.close();
        assert_eq!(barrier.state().active_watcher_tasks, 1);
        cancelled_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("close must wake a watcher waiting for cancellation");
        watcher.join().unwrap();

        let complete = barrier.wait();
        assert_eq!(complete.active_watcher_tasks, 0);
        assert!(complete.complete);
    }

    #[tokio::test]
    async fn scoped_async_lifecycle_waits_wake_and_retain_terminal_state() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("async-lifecycle-root")),
        )
        .unwrap();
        let mut drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        let poll = host.request_poll().unwrap();
        let retained_poll = poll.clone();
        let ready = host.ready_waiter().unwrap();
        let retained_ready = ready.clone();
        let watcher = host.register_watcher_task().unwrap();

        let poll_task = tokio::spawn(async move { poll.wait_async().await });
        let ready_task = tokio::spawn(async move { ready.wait_async().await });
        let watcher_task = tokio::spawn(async move {
            watcher.wait_for_cancellation_async().await;
            watcher
        });
        tokio::task::yield_now().await;

        let barrier = host.close_with_consumer(&mut drain).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), poll_task)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationPollResolution::Cancelled
        );
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), ready_task)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationReadyResolution::Cancelled
        );

        // Terminal state is retained even if the future is not constructed or
        // first polled until after cancellation has already completed.
        assert_eq!(
            retained_poll.wait_async().await,
            ScopedObservationPollResolution::Cancelled
        );
        assert_eq!(
            retained_ready.wait_async().await,
            ScopedObservationReadyResolution::Cancelled
        );

        let watcher = tokio::time::timeout(std::time::Duration::from_secs(2), watcher_task)
            .await
            .unwrap()
            .unwrap();
        assert!(!barrier.state().complete);
        drop(watcher);
        let complete =
            tokio::time::timeout(std::time::Duration::from_secs(2), barrier.wait_async())
                .await
                .unwrap();
        assert!(complete.complete);
        assert_eq!(complete.active_operations, 0);
        assert_eq!(complete.active_watcher_tasks, 0);
        assert!(!complete.consumer_drain_pending);
    }

    #[tokio::test]
    async fn scoped_async_runtime_ends_a_pending_event_wait_on_direct_host_close() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("async-runtime-root")),
        )
        .unwrap();
        let mut runtime = ScopedObservationAsyncRuntime::open(
            host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let handle = runtime.handle();

        let (next, barrier) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(runtime.next_event(), async {
                tokio::task::yield_now().await;
                handle.host().close()
            })
        })
        .await
        .unwrap();
        assert!(next.unwrap().is_none());
        assert!(barrier.wait_async().await.complete);
        assert_eq!(runtime.applied_state().pending_sequence, None);
        assert!(matches!(runtime.next_event().await, Ok(None)));
    }

    #[tokio::test]
    async fn scoped_async_runtime_delivers_and_applies_bootstrap_concurrently_with_ready() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("async-runtime-bootstrap-root")),
        )
        .unwrap();
        let mut runtime = ScopedObservationAsyncRuntime::open(
            host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let mut admission = admission_lane(1, 0, 1);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"async-runtime-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };

        let (poll, poll_watermark) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(handle.poll(), async {
                    let lease = loop {
                        if let Some(lease) = handle.host().begin_poll().unwrap() {
                            break lease;
                        }
                        tokio::task::yield_now().await;
                    };
                    reconcile_missing_poll(
                        handle.host(),
                        &lease,
                        &mut object,
                        &mut admission,
                        &identity,
                        &origin,
                        AccessPhase::Initial,
                    );
                    handle.with_attachment(|host, drain| {
                        host.complete_bootstrap_poll(lease, &admission, &projection, drain)
                            .unwrap()
                    })
                })
            })
            .await
            .unwrap();
        assert!(matches!(
            poll,
            Ok(ScopedObservationPollResolution::Ready(watermark))
                if Arc::ptr_eq(&watermark, &poll_watermark)
        ));
        object.complete_bootstrap().unwrap();

        let (yielded, ready, barrier) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(runtime.next_event(), handle.ready(), async {
                    tokio::task::yield_now().await;
                    handle.with_attachment(|host, drain| {
                        host.offer_consumer_bootstrap_complete(
                            std::slice::from_ref(&object),
                            &admission,
                            &projection,
                            drain,
                            50,
                        )
                        .unwrap()
                    })
                })
            })
            .await
            .unwrap();
        let yielded = yielded.unwrap().unwrap();
        assert!(matches!(
            &yielded.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { barrier: delivered }
                if Arc::ptr_eq(delivered, &barrier)
        ));
        assert!(matches!(
            ready,
            Ok(ScopedObservationReadyResolution::Ready(ready))
                if Arc::ptr_eq(&ready, &barrier)
        ));
        assert_eq!(
            runtime.applied_state().pending_sequence,
            Some(barrier.barrier_sequence)
        );
        let applied = runtime
            .acknowledge_applied(yielded.application_receipt())
            .unwrap();
        assert_eq!(
            applied.bootstrap_barrier_sequence,
            Some(barrier.barrier_sequence)
        );
        assert_eq!(applied.pending_sequence, None);

        assert!(runtime.close().await.complete);
        assert!(runtime.next_event().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn scoped_native_watcher_buffers_registration_callbacks_and_owns_shutdown() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-watch-root");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("session.jsonl");
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut runtime = ScopedObservationAsyncRuntime::open(
            host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let watcher = handle
            .install_native_watcher_with_factory(4, {
                let registrations = Arc::clone(&registrations);
                let target = target.clone();
                let unrelated = root.join("unrelated.tmp");
                move |callback| {
                    Ok(Box::new(ImmediateScopedWatchBackend {
                        callback,
                        target,
                        unrelated,
                        registrations,
                        fail_registration: false,
                    }))
                }
            })
            .unwrap();
        assert_eq!(watcher.watch_anchor_count(), 1);
        assert_eq!(
            watcher.coordinator().phase(),
            ScopedObservationWatcherPhase::WatcherInstalled
        );
        assert_eq!(watcher.state().generation, 1);
        assert!(!watcher.state().backend_failed);
        assert!(!watcher.state().routing_failed);
        let signalled = watcher.waiter().wait_after(0).await;
        assert_eq!(signalled.generation, 1);
        {
            let registered = registrations.lock().unwrap();
            assert_eq!(registered.len(), 1);
            assert_eq!(registered[0].0, root.canonicalize().unwrap());
            assert_eq!(registered[0].1, notify::RecursiveMode::NonRecursive);
        }

        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let mut admission = admission_lane(1, 0, 1);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"native-watch-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let scan = watcher
            .coordinator()
            .begin_initial_scan(handle.host())
            .unwrap();
        let missing = reconcile_scoped_append(
            handle.host(),
            scan.access_pass(),
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Initial,
        );
        assert!(!missing.object_present);
        handle
            .with_attachment(|host, drain| {
                watcher.coordinator().finish_initial_scan(
                    host,
                    scan,
                    &admission,
                    &projection,
                    drain,
                )
            })
            .unwrap();
        assert_eq!(
            watcher.coordinator().phase(),
            ScopedObservationWatcherPhase::ReconcilePending
        );

        let shutdown = tokio::spawn(watcher.run_until_cancelled());
        tokio::task::yield_now().await;
        let barrier = handle.request_close();
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap();
        assert!(barrier.wait_async().await.complete);
        assert!(runtime.next_event().await.unwrap().is_none());
    }

    #[test]
    fn scoped_native_watcher_registration_failure_releases_retry_slot() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-watch-retry-root");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("session.jsonl");
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let runtime = ScopedObservationAsyncRuntime::open(
            host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let failed = handle.install_native_watcher_with_factory(1, {
            let target = target.clone();
            let unrelated = root.join("unrelated.tmp");
            move |callback| {
                Ok(Box::new(ImmediateScopedWatchBackend {
                    callback,
                    target,
                    unrelated,
                    registrations: Arc::new(std::sync::Mutex::new(Vec::new())),
                    fail_registration: true,
                }))
            }
        });
        assert!(matches!(
            failed,
            Err(ScopedObservationNativeWatcherError::AnchorRegistrationFailed { anchor_index: 0 })
        ));

        let recovered = handle
            .install_native_watcher_with_factory(1, {
                let target = target.clone();
                let unrelated = root.join("unrelated.tmp");
                move |callback| {
                    Ok(Box::new(ImmediateScopedWatchBackend {
                        callback,
                        target,
                        unrelated,
                        registrations: Arc::new(std::sync::Mutex::new(Vec::new())),
                        fail_registration: false,
                    }))
                }
            })
            .unwrap();
        assert_eq!(
            recovered.coordinator().phase(),
            ScopedObservationWatcherPhase::WatcherInstalled
        );
        drop(recovered);
        assert!(handle.host().is_closed());
    }

    #[test]
    fn scoped_watcher_before_scan_reconciles_races_before_bootstrap_and_schedules_live_poll() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("watch-before-scan-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 4,
                max_retained_native_bytes: 0,
                max_source_control_items: 4,
            })
            .unwrap();
        let ready_waiter = host.ready_waiter().unwrap();
        assert_eq!(
            ready_waiter.resolution(),
            ScopedObservationReadyResolution::Pending
        );
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let ready_thread = std::thread::spawn(move || {
            ready_tx.send(ready_waiter.wait()).unwrap();
        });
        let event_waiter = drain.event_waiter();
        let initial_event_state = event_waiter.snapshot();
        assert_eq!(initial_event_state.offered_through_sequence, 0);
        assert!(!initial_event_state.closed);
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel(0);
        let event_thread = std::thread::spawn(move || {
            event_tx
                .send(event_waiter.wait_after(initial_event_state.offered_through_sequence))
                .unwrap();
        });
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let mut admission = admission_lane(8, 0, 4);
        let mut projection =
            ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 1,
            })
            .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"watch-before-scan-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };

        // The callback sink and lifecycle registration exist before backend
        // installation. Dropping a failed installation is retryable, while an
        // unconfirmed installation cannot reserve source access.
        let failed_install = host.prepare_watcher_install(1).unwrap();
        assert_eq!(
            failed_install.phase(),
            ScopedObservationWatcherPhase::InstallingWatcher
        );
        assert!(matches!(
            failed_install.begin_initial_scan(&host),
            Err(ScopedObservationStartupError::InvalidPhase {
                phase: ScopedObservationWatcherPhase::InstallingWatcher,
                ..
            })
        ));
        drop(failed_install);

        let startup = host.prepare_watcher_install(1).unwrap();
        assert!(matches!(
            host.prepare_watcher_install(1),
            Err(ScopedObservationStartupError::WatcherAlreadyInstalled)
        ));

        let first_hint = DirtyHint {
            scope: DirtyScope::Object(b"root-object".to_vec()),
            reason: DirtyReason::NativeEvent,
        };
        assert!(matches!(
            startup.record_hint(&host, first_hint).unwrap(),
            ScopedObservationWatcherHintAction::Buffered(HintEnqueue::Added)
        ));
        assert_eq!(
            startup.confirm_watcher_installed(&host).unwrap(),
            ScopedObservationWatcherPhase::WatcherInstalled
        );

        let scan = startup.begin_initial_scan(&host).unwrap();
        assert_eq!(startup.phase(), ScopedObservationWatcherPhase::InitialScan);
        let missing = reconcile_scoped_append(
            &host,
            scan.access_pass(),
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Initial,
        );
        assert!(!missing.object_present);

        // Creation races the initial missing read. With capacity one, a second
        // distinct callback collapses the bounded set to a full-instance pass
        // instead of dropping either signal.
        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        assert!(matches!(
            startup
                .record_hint(
                    &host,
                    DirtyHint {
                        scope: DirtyScope::Object(b"second-callback".to_vec()),
                        reason: DirtyReason::NativeEvent,
                    },
                )
                .unwrap(),
            ScopedObservationWatcherHintAction::Buffered(HintEnqueue::EscalatedToInstance)
        ));
        let initial = startup
            .finish_initial_scan(&host, scan, &admission, &projection, &drain)
            .unwrap();
        assert_eq!(initial.offered_through_sequence, 0);
        assert_eq!(
            startup.phase(),
            ScopedObservationWatcherPhase::ReconcilePending
        );

        // Abandoning a reconcile attempt restores its hints and releases the
        // exact-scope pass, so the raced creation remains retryable.
        let ScopedObservationStartupReconcileAction::Reconcile(abandoned) =
            startup.next_reconcile(&host, 4).unwrap()
        else {
            panic!("the initial scan race must schedule reconciliation");
        };
        assert_eq!(abandoned.hints().len(), 1);
        assert_eq!(
            abandoned.hints()[0].scope,
            DirtyScope::Instance(host.root_identity().source_instance_key.as_bytes().to_vec())
        );
        assert_eq!(
            abandoned.hints()[0].reason,
            DirtyReason::InternalQueueOverflow
        );
        drop(abandoned);

        let ScopedObservationStartupReconcileAction::Reconcile(reconcile) =
            startup.next_reconcile(&host, 4).unwrap()
        else {
            panic!("dropping a startup pass must requeue its hints");
        };
        let created = reconcile_scoped_append(
            &host,
            reconcile.access_pass(),
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Revalidation,
        );
        assert!(created.object_present);
        assert_eq!(
            created.presence_change,
            Some(ScopedAppendPresenceChange::Created { generation: 1 })
        );
        while !admission.is_empty() {
            assert!(host
                .offer_consumer_next(&mut admission, &mut projection, &mut drain)
                .unwrap()
                .is_some());
        }
        let reconciled = startup
            .finish_reconcile(&host, reconcile, &admission, &projection, &drain)
            .unwrap();
        assert_eq!(reconciled.offered_through_sequence, 1);
        let first_event = event_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the first offered envelope must wake the event waiter");
        assert_eq!(first_event.offered_through_sequence, 1);
        assert!(!first_event.closed);
        event_thread.join().unwrap();
        assert!(matches!(
            startup.next_reconcile(&host, 4).unwrap(),
            ScopedObservationStartupReconcileAction::CaughtUp
        ));

        // A callback after the provisional empty check still blocks the
        // barrier and receives another exact pass.
        assert!(matches!(
            startup
                .record_hint(
                    &host,
                    DirtyHint {
                        scope: DirtyScope::Object(b"root-object".to_vec()),
                        reason: DirtyReason::NativeEvent,
                    },
                )
                .unwrap(),
            ScopedObservationWatcherHintAction::Buffered(HintEnqueue::Added)
        ));
        assert!(matches!(
            startup.offer_bootstrap_complete(
                &host,
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut drain,
                50,
            ),
            Err(ScopedObservationStartupError::ReconcilePending { pending_hints: 1 })
        ));
        let ScopedObservationStartupReconcileAction::Reconcile(reconcile) =
            startup.next_reconcile(&host, 4).unwrap()
        else {
            panic!("the finalization race must schedule reconciliation");
        };
        let unchanged = reconcile_scoped_append(
            &host,
            reconcile.access_pass(),
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Revalidation,
        );
        assert!(unchanged.object_present);
        assert!(unchanged.presence_change.is_none());
        assert!(admission.is_empty());
        startup
            .finish_reconcile(&host, reconcile, &admission, &projection, &drain)
            .unwrap();
        assert!(matches!(
            startup.next_reconcile(&host, 4).unwrap(),
            ScopedObservationStartupReconcileAction::CaughtUp
        ));

        object.complete_bootstrap().unwrap();
        let barrier = startup
            .offer_bootstrap_complete(
                &host,
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut drain,
                50,
            )
            .unwrap();
        assert_eq!(barrier.barrier_sequence, 2);
        let ready_resolution = ready_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("bootstrap offer must wake the engine-ready waiter");
        assert!(matches!(
            ready_resolution,
            ScopedObservationReadyResolution::Ready(ready)
                if Arc::ptr_eq(&ready, &barrier)
        ));
        ready_thread.join().unwrap();
        assert_eq!(
            startup.phase(),
            ScopedObservationWatcherPhase::Live {
                offered_through_sequence: 2,
            }
        );
        let repeated = startup
            .offer_bootstrap_complete(
                &host,
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut drain,
                51,
            )
            .unwrap();
        assert!(Arc::ptr_eq(&barrier, &repeated));

        let mut active = host
            .bind_consumer_bootstrap_epoch_state(vec![object], admission, projection, &drain)
            .unwrap();
        let live_ticket = match startup
            .record_hint(
                &host,
                DirtyHint {
                    scope: DirtyScope::Object(b"root-object".to_vec()),
                    reason: DirtyReason::NativeEvent,
                },
            )
            .unwrap()
        {
            ScopedObservationWatcherHintAction::PollRequested { hint, ticket } => {
                assert_eq!(hint.reason, DirtyReason::NativeEvent);
                ticket
            }
            ScopedObservationWatcherHintAction::Buffered(_) => {
                panic!("a post-barrier callback must schedule a live poll")
            }
        };
        let lease = host.begin_poll().unwrap().unwrap();
        let (active_object, active_admission) = active
            .append_object_and_admission_mut("root-object")
            .unwrap();
        let live = reconcile_scoped_append(
            &host,
            lease.access_pass(),
            active_object,
            active_admission,
            &identity,
            &origin,
            AccessPhase::Revalidation,
        );
        assert!(live.object_present);
        assert!(active.admission_is_empty());
        let live_watermark = host.complete_epoch_poll(lease, &active, &drain).unwrap();
        assert_eq!(live_watermark.offered_through_sequence, 2);
        assert!(matches!(
            host.poll_resolution(&live_ticket).unwrap(),
            ScopedObservationPollResolution::Ready(watermark)
                if Arc::ptr_eq(&watermark, &live_watermark)
        ));

        let retained_ready = host.ready_waiter().unwrap();
        let closing_events = drain.event_waiter();
        let before_close = closing_events.snapshot();
        assert_eq!(before_close.offered_through_sequence, 2);
        let (event_close_tx, event_close_rx) = std::sync::mpsc::sync_channel(0);
        let event_close_thread = std::thread::spawn(move || {
            event_close_tx
                .send(closing_events.wait_after(before_close.offered_through_sequence))
                .unwrap();
        });
        let close = host.close_with_consumer(&mut drain).unwrap();
        assert!(matches!(
            live_ticket.wait(),
            ScopedObservationPollResolution::Ready(watermark)
                if Arc::ptr_eq(&watermark, &live_watermark)
        ));
        assert!(matches!(
            retained_ready.wait(),
            ScopedObservationReadyResolution::Ready(ready)
                if Arc::ptr_eq(&ready, &barrier)
        ));
        assert!(startup.cancellation_requested());
        assert_eq!(close.state().active_watcher_tasks, 1);
        let closed_events = event_close_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("drain close must wake the event waiter");
        assert_eq!(
            closed_events.offered_through_sequence,
            before_close.offered_through_sequence
        );
        assert!(closed_events.closed);
        event_close_thread.join().unwrap();
        drop(startup);
        assert!(close.wait().complete);
    }

    #[test]
    fn scoped_consumer_offer_rejects_a_foreign_attachment_without_mutation() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let first = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("first-consumer-offer")),
        )
        .unwrap();
        let second = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("second-consumer-offer")),
        )
        .unwrap();
        let mut drain = first
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        let mut admission = admission_lane(1, 0, 1);
        let mut projection =
            ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 1,
            })
            .unwrap();
        let before = drain.delivery_lane().state();

        assert!(matches!(
            second.offer_consumer_next(&mut admission, &mut projection, &mut drain),
            Err(ScopedObservationConsumerOfferError::ForeignDrain)
        ));
        assert_eq!(drain.delivery_lane().state(), before);
    }

    #[test]
    fn scoped_root_identity_is_resolved_and_validated_before_attachment_access() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root-identity");
        let request = scoped_access_request(root.clone());
        let debug = format!("{:?}", request.root_identity);
        assert!(!debug.contains("fixture-source-instance"));
        assert!(!debug.contains("fixture-session"));

        // Root identity resolution does not require the transcript or even its
        // containing directory to exist.
        assert!(!root.exists());
        let baseline = ScopedObservationAccessHost::authorize(&registry, request.clone()).unwrap();
        let baseline_root = baseline.root_identity().clone();
        assert_eq!(baseline_root.adapter_id.as_str(), "fixture");
        assert_eq!(
            baseline_root.session_ref.entity_key,
            baseline_root.session_key
        );
        assert_ne!(baseline_root.session_key, baseline_root.root_actor_run_key);
        let durable_batch =
            FactBatch::new_with_semantic_context(2, 2, fixture_semantic_context()).unwrap();
        assert_eq!(
            baseline_root.session_key,
            durable_batch
                .canonical_entity_key("session", b"fixture-session")
                .unwrap()
        );
        assert_eq!(
            baseline_root.root_actor_run_key,
            durable_batch
                .canonical_root_actor_run_key(b"fixture-session", None)
                .unwrap()
        );
        assert!(!root.exists());

        let mut matched_request = request.clone();
        matched_request.root_identity = ScopedRootIdentityRequest::new(
            1,
            b"fixture-source-instance".as_slice(),
            b"fixture-session".as_slice(),
            Some(Arc::from(b"explicit-fixture-root-run".as_slice())),
            Some(baseline_root.session_key),
            Some(baseline_root.session_ref),
        );
        let matched = ScopedObservationAccessHost::authorize(&registry, matched_request).unwrap();
        assert_eq!(
            matched.root_identity().session_key,
            baseline_root.session_key
        );
        assert_eq!(
            matched.root_identity().session_ref,
            baseline_root.session_ref
        );
        assert_ne!(
            matched.root_identity().root_actor_run_key,
            baseline_root.root_actor_run_key
        );
        assert_eq!(
            matched.root_identity().root_actor_run_key,
            durable_batch
                .canonical_root_actor_run_key(
                    b"fixture-session",
                    Some(b"explicit-fixture-root-run")
                )
                .unwrap()
        );
        assert!(!root.exists());

        let mut wrong_key_request = request.clone();
        wrong_key_request.root_identity = ScopedRootIdentityRequest::new(
            1,
            b"fixture-source-instance".as_slice(),
            b"fixture-session".as_slice(),
            None,
            Some(baseline_root.root_actor_run_key),
            Some(baseline_root.session_ref),
        );
        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, wrong_key_request),
            Err(ScopedObservationAccessError::InvalidRootIdentity)
        ));

        let mut wrong_ref_request = request;
        wrong_ref_request.root_identity = ScopedRootIdentityRequest::new(
            1,
            b"fixture-source-instance".as_slice(),
            b"fixture-session".as_slice(),
            None,
            Some(baseline_root.session_key),
            Some(ExternalEntityRef::new(baseline_root.root_actor_run_key)),
        );
        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, wrong_ref_request),
            Err(ScopedObservationAccessError::InvalidRootIdentity)
        ));
        assert!(!root.exists());
    }

    #[test]
    fn scoped_host_requires_exactly_one_designated_root_object() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let mut request = scoped_access_request(temp.path().join("missing-root-role"));
        request.known_objects[0].scope_root = false;

        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, request),
            Err(ScopedObservationAccessError::InvalidGrant(message))
                if message.contains("exactly one") && message.contains("scope root")
        ));
    }

    #[test]
    fn scoped_append_rejects_a_foreign_source_before_reserving_access() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("foreign-source-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"must not be read\n").unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let mut object = ScopedKnownAppendObject::new(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            ScopedAppendDecoderConfig {
                decoder: DecoderId::new("fixture-decoder").unwrap(),
                object_context: AdapterObjectContext::empty(),
                semantic_context: fixture_semantic_context_for_source(b"foreign-source-instance"),
                coverage_domains: Vec::new(),
                retention: RawRetentionPolicy::None,
                max_facts_per_record: 16,
                max_diagnostics_per_record: 16,
            },
        )
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"foreign-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let pass = host.begin_pass().unwrap();
        assert!(matches!(
            object.reconcile(
                &pass,
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    access_phase: AccessPhase::Initial,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 128,
                    origin: &origin,
                    force_contract_replay: false,
                },
            ),
            Err(ScopedObservationAccessError::InvalidRootIdentity)
        ));
        let report = pass.report();
        assert_eq!(report.relations()[0].attempts, 0);
        assert_eq!(report.relations()[0].bytes_read, 0);
        assert!(object.checkpoint().is_none());
        assert!(!object.is_present());
        assert_eq!(object.relation_id(), None);
    }

    #[test]
    fn scoped_barrier_rejects_an_authorized_relation_without_observed_coverage() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("unobserved-relation-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let admission = admission_lane(1, 0, 1);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let mut delivery = ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
        let initial_state = delivery.state();

        assert!(matches!(
            host.capture_watermark_core(&admission, &projection, &delivery),
            Err(ScopedCoverageAssemblyError::DeclaredObjectCoverageMismatch)
        ));
        assert!(matches!(
            host.offer_bootstrap_complete(&[], &admission, &projection, &mut delivery, 50,),
            Err(ScopedBootstrapBarrierError::Coverage(
                ScopedCoverageAssemblyError::DeclaredObjectCoverageMismatch
            ))
        ));
        assert_eq!(delivery.state(), initial_state);
        assert!(delivery.bootstrap_barrier().is_none());
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_admission_rejects_two_source_objects_claiming_one_relation() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("duplicate-relation-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let mut first = scoped_append_object(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
        );
        let mut second = ScopedKnownAppendObject::new(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            ScopedAppendDecoderConfig {
                decoder: DecoderId::new("fixture-decoder").unwrap(),
                object_context: AdapterObjectContext::empty(),
                semantic_context: FactSemanticContext::new(
                    &AdapterId::new("fixture").unwrap(),
                    1,
                    b"fixture-source-instance",
                    b"fixture-second-transcript",
                    b"second-session.jsonl",
                    1,
                )
                .unwrap(),
                coverage_domains: Vec::new(),
                retention: RawRetentionPolicy::None,
                max_facts_per_record: 16,
                max_diagnostics_per_record: 16,
            },
        )
        .unwrap();
        assert_ne!(first.source_identity(), second.source_identity());
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"duplicate-relation-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let request = || ScopedAppendReconcileRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            access_phase: AccessPhase::Initial,
            parent_token: None,
            depth: 1,
            max_bytes: 64,
            origin: &origin,
            force_contract_replay: false,
        };
        let mut admission = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events: 1,
            max_retained_native_bytes: 0,
            max_control_items: 1,
            max_coverage_objects: 2,
        })
        .unwrap();

        let first_pass = host.begin_pass().unwrap();
        let first_observation = first.reconcile(&first_pass, request()).unwrap();
        let ScopedAppendDecodeOutcome::Ready(first_decoded) =
            decode_scoped(&host, &mut first, &first_observation)
        else {
            panic!("missing first source should decode to an empty batch");
        };
        if let Err(failure) = admission.admit(&mut first, &first_observation, first_decoded) {
            panic!("first admission failed: {}", failure.error);
        }
        drop(first_pass);
        assert_eq!(first.relation_id(), Some("root-object"));

        let second_pass = host.begin_pass().unwrap();
        let second_observation = second.reconcile(&second_pass, request()).unwrap();
        let ScopedAppendDecodeOutcome::Ready(second_decoded) =
            decode_scoped(&host, &mut second, &second_observation)
        else {
            panic!("missing second source should decode to an empty batch");
        };
        let failure = admission
            .admit(&mut second, &second_observation, second_decoded)
            .expect_err("one relation cannot account for two semantic source objects");
        assert_eq!(failure.error, ScopedAdmissionError::InvalidCoverage);
        assert_eq!(second.relation_id(), Some("root-object"));
        second.discard(&second_observation).unwrap();
        assert!(second.checkpoint().is_none());
        assert!(!second.is_present());
        assert_eq!(second_pass.finish().relations()[0].attempts, 1);
    }

    #[test]
    fn scoped_append_decoder_binding_rejects_unbounded_output_configuration() {
        let result = ScopedKnownAppendObject::new(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            ScopedAppendDecoderConfig {
                decoder: DecoderId::new("fixture-decoder").unwrap(),
                object_context: AdapterObjectContext::empty(),
                semantic_context: fixture_semantic_context(),
                coverage_domains: Vec::new(),
                retention: RawRetentionPolicy::None,
                max_facts_per_record: 0,
                max_diagnostics_per_record: 16,
            },
        );
        assert!(matches!(
            result,
            Err(ScopedObservationAccessError::InvalidDecodeBounds)
        ));
    }

    #[test]
    fn scoped_append_decoder_rejects_non_fact_and_duplicate_coverage_domains() {
        let usage = CoverageDomain::FactFamily {
            family: "runtime.usage-v2".to_string(),
            version: 1,
        };
        for coverage_domains in [
            vec![CoverageDomain::Decode],
            vec![CoverageDomain::ProjectionPack {
                pack: "history".to_string(),
                version: 1,
            }],
            vec![usage.clone(), usage.clone()],
        ] {
            let result = ScopedKnownAppendObject::new(
                AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
                ScopedAppendDecoderConfig {
                    decoder: DecoderId::new("fixture-decoder").unwrap(),
                    object_context: AdapterObjectContext::empty(),
                    semantic_context: fixture_semantic_context(),
                    coverage_domains,
                    retention: RawRetentionPolicy::None,
                    max_facts_per_record: 16,
                    max_diagnostics_per_record: 16,
                },
            );
            assert!(matches!(
                result,
                Err(ScopedObservationAccessError::InvalidCoverageDomains)
            ));
        }
    }

    #[test]
    fn scoped_append_kernel_keeps_cursor_partial_and_reset_state_without_a_store() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("append-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 128;
        config.max_batch_bytes = 128;
        config.max_records_per_batch = 16;
        config.prefix_anchor_bytes = 16;
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(config).unwrap(),
            RawRetentionPolicy::None,
        );
        assert_eq!(object.relation_id(), None);
        assert!(matches!(
            object.complete_bootstrap(),
            Err(ScopedObservationAccessError::BootstrapNotDrained)
        ));
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"private-session-id",
        }];
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let reconcile = |max_bytes| ScopedAppendReconcileRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            access_phase: AccessPhase::Initial,
            parent_token: None,
            depth: 1,
            max_bytes,
            origin: &origin,
            force_contract_replay: false,
        };

        let missing_pass = host.begin_pass().unwrap();
        let missing = object.reconcile(&missing_pass, reconcile(64)).unwrap();
        assert_eq!(object.relation_id(), Some("root-object"));
        assert_eq!(missing.phase, ScopedAppendDeliveryPhase::Bootstrap);
        assert!(!missing.object_present);
        assert!(missing.presence_change.is_none());
        assert!(matches!(&missing.read, AppendRead::Missing));
        decode_and_admit_ignored(&host, &mut object, &missing);
        assert_eq!(
            missing_pass.finish().relations()[0].trace[0].outcome,
            AccessOutcome::Unavailable
        );
        object.complete_bootstrap().unwrap();
        assert!(matches!(
            object.complete_bootstrap(),
            Err(ScopedObservationAccessError::BootstrapAlreadyComplete)
        ));

        let wrong_relation_pass = host.begin_pass().unwrap();
        assert!(matches!(
            object.reconcile(
                &wrong_relation_pass,
                ScopedAppendReconcileRequest {
                    relation_id: "other-object",
                    identity_inputs: &identity,
                    access_phase: AccessPhase::Initial,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin: &origin,
                    force_contract_replay: false,
                },
            ),
            Err(ScopedObservationAccessError::InvalidGrant(_))
        ));
        assert_eq!(object.relation_id(), Some("root-object"));
        assert_eq!(wrong_relation_pass.finish().relations()[0].attempts, 0);

        std::fs::write(root.join("session.jsonl"), b"one\npartial").unwrap();
        let limited_pass = host.begin_pass().unwrap();
        assert!(matches!(
            object.reconcile(&limited_pass, reconcile(4)),
            Err(ScopedObservationAccessError::Source(
                ScopedSourceFailureClass::LimitExceeded
            ))
        ));
        let limited_report = limited_pass.finish();
        assert_eq!(limited_report.relations()[0].bytes_read, 4);
        assert_eq!(
            limited_report.relations()[0].trace[0].outcome,
            AccessOutcome::Failed
        );

        let initial_pass = host.begin_pass().unwrap();
        let initial = object.reconcile(&initial_pass, reconcile(64)).unwrap();
        assert_eq!(initial.phase, ScopedAppendDeliveryPhase::Live);
        assert!(initial.reset_before_items.is_none());
        assert_eq!(
            initial.presence_change,
            Some(ScopedAppendPresenceChange::Created { generation: 1 })
        );
        let AppendRead::Batch {
            items,
            checkpoint,
            transition,
            needs_retry,
            more_available,
            bytes_read,
            ..
        } = &initial.read
        else {
            panic!("expected initial append batch");
        };
        assert_eq!(*transition, AppendTransition::Initial);
        assert!(*needs_retry);
        assert!(!*more_available);
        assert_eq!(checkpoint.incomplete_suffix_len, 7);
        assert_eq!(*bytes_read, 15);
        let AppendItem::Record(record) = &items[0] else {
            panic!("expected framed record");
        };
        assert_eq!(record.payload, b"one");
        assert!(object.checkpoint().is_none());
        object.discard(&initial).unwrap();
        assert_eq!(initial_pass.finish().relations()[0].bytes_read, *bytes_read);

        let replay_pass = host.begin_pass().unwrap();
        let replay = object.reconcile(&replay_pass, reconcile(64)).unwrap();
        assert_eq!(replay.phase, ScopedAppendDeliveryPhase::Live);
        assert_eq!(&replay.read, &initial.read);
        decode_and_admit_ignored(&host, &mut object, &replay);
        assert_eq!(object.checkpoint().unwrap().committed_offset, 4);
        drop(replay_pass);

        let mut append = OpenOptions::new()
            .append(true)
            .open(root.join("session.jsonl"))
            .unwrap();
        append.write_all(b"-done\n").unwrap();
        append.flush().unwrap();
        let continued_pass = host.begin_pass().unwrap();
        let continued = object.reconcile(&continued_pass, reconcile(64)).unwrap();
        assert_eq!(continued.phase, ScopedAppendDeliveryPhase::Live);
        assert!(continued.presence_change.is_none());
        let AppendRead::Batch {
            items,
            transition,
            checkpoint,
            ..
        } = &continued.read
        else {
            panic!("expected continued append batch");
        };
        assert_eq!(*transition, AppendTransition::Continued);
        assert_eq!(checkpoint.generation, 1);
        let AppendItem::Record(record) = &items[0] else {
            panic!("expected completed partial record");
        };
        assert_eq!(record.payload, b"partial-done");
        decode_and_admit_ignored(&host, &mut object, &continued);
        drop(continued_pass);

        std::fs::write(root.join("session.jsonl"), b"replacement\n").unwrap();
        let correction_pass = host.begin_pass().unwrap();
        let correction = object.reconcile(&correction_pass, reconcile(64)).unwrap();
        assert_eq!(correction.phase, ScopedAppendDeliveryPhase::Correction);
        assert!(correction.presence_change.is_none());
        let reset = correction.reset_before_items.unwrap();
        assert_eq!(reset.old_generation, 1);
        assert_eq!(reset.new_generation, 2);
        assert_eq!(reset.reason, AppendTransition::Truncated);
        let AppendRead::Batch { items, .. } = &correction.read else {
            panic!("expected correction batch");
        };
        let AppendItem::Record(record) = &items[0] else {
            panic!("expected replacement record");
        };
        assert_eq!(record.generation, 2);
        assert_eq!(record.payload, b"replacement");
        decode_and_admit_ignored(&host, &mut object, &correction);
        assert_eq!(object.checkpoint().unwrap().generation, 2);
        assert!(object.is_present());
        assert!(!object.bootstrap_active());
        drop(correction_pass);

        std::fs::remove_file(root.join("session.jsonl")).unwrap();
        let deletion_pass = host.begin_pass().unwrap();
        let deletion = object.reconcile(&deletion_pass, reconcile(64)).unwrap();
        assert_eq!(deletion.phase, ScopedAppendDeliveryPhase::Live);
        assert!(deletion.became_missing);
        assert_eq!(
            deletion.presence_change,
            Some(ScopedAppendPresenceChange::Deleted { generation: 2 })
        );
        let ScopedAppendDecodeOutcome::Ready(deletion_decoded) =
            decode_scoped(&host, &mut object, &deletion)
        else {
            panic!("missing source should decode to an empty admitted batch");
        };
        let mut deletion_lane = admission_lane(4, 0, 4);
        let deletion_receipt = match deletion_lane.admit(&mut object, &deletion, deletion_decoded) {
            Ok(receipt) => receipt,
            Err(failure) => panic!("deletion admission failed: {}", failure.error),
        };
        assert_eq!(deletion_receipt.control_items, 1);
        assert_eq!(deletion_receipt.data_events, 0);
        let expected_source = object.source_identity().clone();
        assert!(matches!(
            deletion_lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Presence {
                source,
                change: ScopedAppendPresenceChange::Deleted { generation: 2 },
                ..
            }) if source == expected_source
        ));
        assert!(deletion_lane.is_empty());
        assert!(!object.is_present());
        drop(deletion_pass);

        std::fs::write(root.join("session.jsonl"), b"recreated\n").unwrap();
        let recreation_pass = host.begin_pass().unwrap();
        let recreation = object.reconcile(&recreation_pass, reconcile(64)).unwrap();
        assert_eq!(recreation.phase, ScopedAppendDeliveryPhase::Correction);
        assert_eq!(
            recreation.presence_change,
            Some(ScopedAppendPresenceChange::Created { generation: 3 })
        );
        assert_eq!(
            recreation.reset_before_items,
            Some(crate::scoped_observation::ScopedAppendReset {
                old_generation: 2,
                new_generation: 3,
                reason: AppendTransition::IdentityChanged,
            })
        );
        let ScopedAppendDecodeOutcome::Ready(recreation_decoded) =
            decode_scoped(&host, &mut object, &recreation)
        else {
            panic!("recreated source should decode one correction batch");
        };
        let mut recreation_lane = admission_lane(4, 0, 4);
        let recreation_receipt =
            match recreation_lane.admit(&mut object, &recreation, recreation_decoded) {
                Ok(receipt) => receipt,
                Err(failure) => panic!("recreation admission failed: {}", failure.error),
            };
        assert_eq!(recreation_receipt.control_items, 2);
        assert!(matches!(
            recreation_lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Presence {
                lane_ordinal: 1,
                change: ScopedAppendPresenceChange::Created { generation: 3 },
                ..
            })
        ));
        assert!(matches!(
            recreation_lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Reset {
                lane_ordinal: 2,
                reset: crate::scoped_observation::ScopedAppendReset {
                    old_generation: 2,
                    new_generation: 3,
                    reason: AppendTransition::IdentityChanged,
                },
                ..
            })
        ));
        assert!(matches!(
            recreation_lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Decoded {
                lane_ordinal: 3,
                ..
            })
        ));
        assert!(recreation_lane.is_empty());
        assert!(object.is_present());
        assert_eq!(object.checkpoint().unwrap().generation, 3);
    }

    #[test]
    fn scoped_append_replacement_isolates_cursor_and_decoder_state_until_swap() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("append-replacement-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"old\n").unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 64;
        config.max_batch_bytes = 64;
        config.max_records_per_batch = 1;
        config.prefix_anchor_bytes = 16;
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(config).unwrap(),
            RawRetentionPolicy::None,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"replacement-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let request = || ScopedAppendReconcileRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            access_phase: AccessPhase::Initial,
            parent_token: None,
            depth: 1,
            max_bytes: 128,
            origin: &origin,
            force_contract_replay: false,
        };

        let bootstrap_pass = host.begin_pass().unwrap();
        let bootstrap = object.reconcile(&bootstrap_pass, request()).unwrap();
        assert_eq!(bootstrap.phase, ScopedAppendDeliveryPhase::Bootstrap);
        decode_and_admit_ignored(&host, &mut object, &bootstrap);
        drop(bootstrap_pass);
        object.complete_bootstrap().unwrap();
        assert_eq!(object.checkpoint().unwrap().committed_offset, 4);
        assert_eq!(object.decoder_state(), Some(b"old".as_slice()));
        assert_eq!(object.replacement_scope_epoch(), None);
        assert!(!object.is_retired());

        std::fs::write(root.join("session.jsonl"), b"new\nmore\n").unwrap();
        let mut abandoned = object.fork_replacement(2).unwrap();
        assert_eq!(abandoned.relation_id(), Some("root-object"));
        assert_eq!(abandoned.replacement_scope_epoch(), Some(2));
        assert_eq!(object.frozen_scope_epoch(), Some(2));
        assert!(abandoned.checkpoint().is_none());
        assert!(abandoned.decoder_state().is_none());
        assert!(matches!(
            object.fork_replacement(1),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));
        assert!(matches!(
            abandoned.fork_replacement(3),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));
        assert!(matches!(
            abandoned.complete_bootstrap(),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));
        let frozen_pass = host.begin_pass().unwrap();
        assert!(matches!(
            object.reconcile(&frozen_pass, request()),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));
        assert_eq!(frozen_pass.finish().relations()[0].attempts, 0);

        let abandoned_pass = host.begin_pass().unwrap();
        let abandoned_observation = abandoned.reconcile(&abandoned_pass, request()).unwrap();
        assert_eq!(
            abandoned_observation.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert!(abandoned_observation.reset_before_items.is_none());
        decode_and_admit_ignored(&host, &mut abandoned, &abandoned_observation);
        drop(abandoned_pass);
        assert_eq!(abandoned.checkpoint().unwrap().committed_offset, 4);
        assert_eq!(abandoned.decoder_state(), Some(b"new".as_slice()));
        assert!(matches!(
            object.validate_replacement_activation(&abandoned, 2),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));

        let abandoned_drain_pass = host.begin_pass().unwrap();
        let abandoned_drain = abandoned
            .reconcile(&abandoned_drain_pass, request())
            .unwrap();
        assert_eq!(abandoned_drain.phase, ScopedAppendDeliveryPhase::Correction);
        decode_and_admit_ignored(&host, &mut abandoned, &abandoned_drain);
        drop(abandoned_drain_pass);
        assert_eq!(abandoned.checkpoint().unwrap().committed_offset, 9);
        assert_eq!(abandoned.decoder_state(), Some(b"newmore".as_slice()));
        object
            .validate_replacement_activation(&abandoned, 2)
            .unwrap();

        // Replay mutation is isolated: abandoning epoch 2 cannot advance the
        // cursor or decoder state still serving the valid old epoch.
        assert_eq!(object.checkpoint().unwrap().committed_offset, 4);
        assert_eq!(object.decoder_state(), Some(b"old".as_slice()));
        assert!(matches!(
            object.validate_replacement_activation(&abandoned, 3),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));

        let mut replacement = object.fork_replacement(3).unwrap();
        assert_eq!(object.frozen_scope_epoch(), Some(3));
        assert!(matches!(
            object.validate_replacement_activation(&abandoned, 2),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));
        let replacement_pass = host.begin_pass().unwrap();
        let replacement_observation = replacement.reconcile(&replacement_pass, request()).unwrap();
        assert_eq!(
            replacement_observation.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        decode_and_admit_ignored(&host, &mut replacement, &replacement_observation);
        drop(replacement_pass);
        assert!(matches!(
            object.validate_replacement_activation(&replacement, 3),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));

        let replacement_drain_pass = host.begin_pass().unwrap();
        let replacement_drain = replacement
            .reconcile(&replacement_drain_pass, request())
            .unwrap();
        assert_eq!(
            replacement_drain.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        decode_and_admit_ignored(&host, &mut replacement, &replacement_drain);
        drop(replacement_drain_pass);
        assert_eq!(replacement.checkpoint().unwrap().committed_offset, 9);
        assert_eq!(replacement.decoder_state(), Some(b"newmore".as_slice()));
        object
            .validate_replacement_activation(&replacement, 3)
            .unwrap();
        object.activate_replacement(&mut replacement, 3).unwrap();

        assert_eq!(object.checkpoint().unwrap().committed_offset, 9);
        assert_eq!(object.decoder_state(), Some(b"newmore".as_slice()));
        assert_eq!(object.replacement_scope_epoch(), None);
        assert_eq!(object.frozen_scope_epoch(), None);
        assert!(!object.bootstrap_active());
        assert!(!object.is_retired());
        assert_eq!(replacement.checkpoint().unwrap().committed_offset, 4);
        assert_eq!(replacement.decoder_state(), Some(b"old".as_slice()));
        assert!(replacement.is_retired());

        let retired_pass = host.begin_pass().unwrap();
        assert!(matches!(
            replacement.reconcile(&retired_pass, request()),
            Err(ScopedObservationAccessError::InvalidObjectLifecycle)
        ));
        assert_eq!(retired_pass.finish().relations()[0].attempts, 0);
    }

    #[test]
    fn scoped_decode_coverage_advances_only_after_the_matching_offer_boundary() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("offered-coverage-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 64;
        config.max_batch_bytes = 64;
        config.max_records_per_batch = 4;
        config.prefix_anchor_bytes = 16;
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(config).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let source = object.source_identity().clone();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"offered-coverage-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let request = || ScopedAppendReconcileRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            access_phase: AccessPhase::Initial,
            parent_token: None,
            depth: 1,
            max_bytes: 64,
            origin: &origin,
            force_contract_replay: false,
        };
        let mut lane = admission_lane(8, 0, 2);
        let mut projection =
            ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 1,
            })
            .unwrap();
        let mut delivery = ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();

        // A stable missing object produces no event, so its explicit absence
        // is already covered by the current (zero) offered sequence.
        let missing_pass = host.begin_pass().unwrap();
        let missing = object.reconcile(&missing_pass, request()).unwrap();
        let ScopedAppendDecodeOutcome::Ready(missing_decoded) =
            decode_scoped(&host, &mut object, &missing)
        else {
            panic!("missing source should decode to an empty batch");
        };
        let missing_receipt = match lane.admit(&mut object, &missing, missing_decoded) {
            Ok(receipt) => receipt,
            Err(failure) => panic!("missing admission failed: {}", failure.error),
        };
        assert_eq!(missing_receipt.through_lane_ordinal, 0);
        assert_eq!(lane.queued_coverage_updates(), 0);
        let missing_coverage = lane.offered_decode_coverage(&source).unwrap();
        assert_eq!(
            missing_coverage.completeness,
            CoverageSetCompleteness::Complete
        );
        assert!(missing_coverage.point.is_none());
        assert!(matches!(
            missing_coverage.explicit_absence_or_deletion,
            Some(crate::adapter::CoverageAbsence {
                generation: 1,
                kind: CoverageAbsenceKind::Absent,
                ..
            })
        ));
        let missing_watermark = host
            .capture_watermark_core(&lane, &projection, &delivery)
            .unwrap();
        assert_eq!(missing_watermark.root, host.root_identity().clone());
        assert_eq!(missing_watermark.scope_epoch, 1);
        assert_eq!(missing_watermark.offered_through_sequence, 0);
        assert!(missing_watermark.explicit_object_errors.is_empty());
        assert_eq!(missing_watermark.source_coverage.len(), 2);
        let missing_set = missing_watermark
            .source_coverage
            .iter()
            .find(|set| set.coverage_domain == CoverageDomain::Decode)
            .unwrap();
        let missing_usage_set = missing_watermark
            .source_coverage
            .iter()
            .find(|set| {
                set.coverage_domain
                    == (CoverageDomain::FactFamily {
                        family: "runtime.usage-v2".to_string(),
                        version: 1,
                    })
            })
            .unwrap();
        assert_eq!(missing_set.coverage_domain, CoverageDomain::Decode);
        assert_eq!(missing_set.scope.adapter_id, "fixture");
        assert_eq!(
            missing_set.scope.source_instance_key,
            host.root_identity().source_instance_key
        );
        assert_eq!(
            missing_set.scope.root_entity_key,
            Some(host.root_identity().session_key)
        );
        assert_eq!(missing_set.scope.support_release_id, "fixture-release");
        assert_eq!(missing_set.completeness, CoverageSetCompleteness::Complete);
        assert_eq!(missing_set.explicit_absence_or_deletion.len(), 1);
        assert_eq!(
            missing_usage_set.explicit_absence_or_deletion,
            missing_set.explicit_absence_or_deletion
        );
        assert_ne!(
            missing_usage_set.membership_revision,
            missing_set.membership_revision
        );
        drop(missing_pass);
        object.complete_bootstrap().unwrap();

        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        let creation_pass = host.begin_pass().unwrap();
        let creation = object.reconcile(&creation_pass, request()).unwrap();
        assert_eq!(
            creation.presence_change,
            Some(ScopedAppendPresenceChange::Created { generation: 1 })
        );
        let ScopedAppendDecodeOutcome::Ready(creation_decoded) =
            decode_scoped(&host, &mut object, &creation)
        else {
            panic!("created source should decode one batch");
        };
        let creation_receipt = match lane.admit(&mut object, &creation, creation_decoded) {
            Ok(receipt) => receipt,
            Err(failure) => panic!("creation admission failed: {}", failure.error),
        };
        assert_eq!(creation_receipt.through_lane_ordinal, 2);
        assert_eq!(object.checkpoint().unwrap().committed_offset, 4);
        assert_eq!(lane.queued_coverage_updates(), 1);
        assert!(matches!(
            host.capture_watermark_core(&lane, &projection, &delivery),
            Err(ScopedCoverageAssemblyError::AdmissionNotDrained)
        ));
        assert!(lane
            .offered_decode_coverage(&source)
            .unwrap()
            .point
            .is_none());

        // Offering source.created is not enough: the read's decoded frame is
        // still pending, so coverage must remain at the prior absence.
        let created_offer = lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(created_offer.offered_through_sequence, 1);
        assert_eq!(lane.queued_coverage_updates(), 1);
        assert!(lane
            .offered_decode_coverage(&source)
            .unwrap()
            .point
            .is_none());

        // Ignored-known decode emits no semantic event, but accepting its
        // frame completes source coverage at the same observer sequence.
        let decoded_offer = lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(decoded_offer.first_offered_sequence, None);
        assert_eq!(decoded_offer.offered_through_sequence, 1);
        assert_eq!(lane.queued_coverage_updates(), 0);
        let present_coverage = lane.offered_decode_coverage(&source).unwrap();
        assert_eq!(
            present_coverage.completeness,
            CoverageSetCompleteness::Complete
        );
        assert!(present_coverage.explicit_absence_or_deletion.is_none());
        assert!(present_coverage.explicit_errors.is_empty());
        let point = present_coverage.point.as_ref().unwrap();
        assert_eq!(point.generation, 1);
        assert_eq!(point.status, CoverageStatus::CompleteThrough);
        assert_eq!(
            point.position.as_ref().unwrap().kind,
            CoveragePositionKind::AppendCursor
        );
        assert_eq!(point.position.as_ref().unwrap().monotonic_order, Some(4));
        let present_watermark = host
            .capture_watermark_core(&lane, &projection, &delivery)
            .unwrap();
        assert_eq!(present_watermark.offered_through_sequence, 1);
        assert_eq!(present_watermark.queue_state.queued_source_control_items, 1);
        assert_eq!(present_watermark.source_coverage.len(), 2);
        let present_set = present_watermark
            .source_coverage
            .iter()
            .find(|set| set.coverage_domain == CoverageDomain::Decode)
            .unwrap();
        let present_usage_set = present_watermark
            .source_coverage
            .iter()
            .find(|set| {
                set.coverage_domain
                    == (CoverageDomain::FactFamily {
                        family: "runtime.usage-v2".to_string(),
                        version: 1,
                    })
            })
            .unwrap();
        assert_eq!(present_set.points.len(), 1);
        assert!(present_set.explicit_absence_or_deletion.is_empty());
        assert_eq!(present_usage_set.points.len(), 1);
        assert_eq!(
            present_usage_set.points[0].position,
            present_set.points[0].position
        );
        assert_ne!(
            present_set.membership_revision,
            missing_set.membership_revision
        );
        drop(creation_pass);

        // Keep source.created in the one-slot control queue, then prove a
        // deletion offer rejected by pressure cannot advance coverage.
        std::fs::remove_file(root.join("session.jsonl")).unwrap();
        let deletion_pass = host.begin_pass().unwrap();
        let deletion = object.reconcile(&deletion_pass, request()).unwrap();
        let ScopedAppendDecodeOutcome::Ready(deletion_decoded) =
            decode_scoped(&host, &mut object, &deletion)
        else {
            panic!("deleted source should decode to an empty batch");
        };
        if let Err(failure) = lane.admit(&mut object, &deletion, deletion_decoded) {
            panic!("deletion admission failed: {}", failure.error);
        }
        assert_eq!(lane.queued_coverage_updates(), 1);
        assert_eq!(
            lane.offer_next(&mut projection, &mut delivery),
            Err(ScopedProjectionDeliveryError::Delivery(
                ScopedDeliveryError::SourceControlQueueFull
            ))
        );
        assert!(lane
            .offered_decode_coverage(&source)
            .unwrap()
            .point
            .is_some());
        assert_eq!(lane.queued_coverage_updates(), 1);
        assert!(matches!(
            host.capture_watermark_core(&lane, &projection, &delivery),
            Err(ScopedCoverageAssemblyError::AdmissionNotDrained)
        ));

        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);
        let deleted_offer = lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(deleted_offer.offered_through_sequence, 2);
        let deleted_coverage = lane.offered_decode_coverage(&source).unwrap();
        assert_eq!(
            deleted_coverage.completeness,
            CoverageSetCompleteness::Complete
        );
        assert!(deleted_coverage.point.is_none());
        assert!(matches!(
            deleted_coverage.explicit_absence_or_deletion,
            Some(crate::adapter::CoverageAbsence {
                generation: 1,
                kind: CoverageAbsenceKind::Deleted,
                ..
            })
        ));
        assert_eq!(lane.queued_coverage_updates(), 0);
        assert!(lane.is_empty());
        let deleted_watermark = host
            .capture_watermark_core(&lane, &projection, &delivery)
            .unwrap();
        assert_eq!(deleted_watermark.offered_through_sequence, 2);
        assert_eq!(deleted_watermark.source_coverage.len(), 2);
        for set in &deleted_watermark.source_coverage {
            assert_eq!(set.points.len(), 0);
            assert_eq!(set.explicit_absence_or_deletion.len(), 1);
        }
        drop(deletion_pass);
    }

    #[test]
    fn scoped_decode_transaction_commits_cursor_and_decoder_state_together() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("decoded-append-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"one\ntwo\n").unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 128;
        config.max_batch_bytes = 128;
        config.max_records_per_batch = 16;
        config.prefix_anchor_bytes = 16;
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(config).unwrap(),
            RawRetentionPolicy::HashOnly,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"decoded-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 100,
            stream_id: 200,
            object_id: 300,
            observed_at: 400,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let request = || ScopedAppendReconcileRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            access_phase: AccessPhase::Initial,
            parent_token: None,
            depth: 1,
            max_bytes: 128,
            origin: &origin,
            force_contract_replay: false,
        };

        let first_pass = host.begin_pass().unwrap();
        let first = object.reconcile(&first_pass, request()).unwrap();
        let ScopedAppendDecodeOutcome::Ready(first_decoded) =
            decode_scoped(&host, &mut object, &first)
        else {
            panic!("expected decoded append batch");
        };
        assert_eq!(first_decoded.items.len(), 2);
        let first_fact_ids = first_decoded
            .items
            .iter()
            .map(|item| {
                let ScopedDecodedAppendItem::Record {
                    evidence,
                    disposition,
                    batch,
                    ..
                } = item
                else {
                    panic!("expected decoded record");
                };
                assert_eq!(*disposition, DecodeDisposition::PreservedUnknown);
                assert!(evidence.retained_payload.is_none());
                let Fact::UnknownRecord { raw_payload, .. } = &batch.facts()[0].value else {
                    panic!("expected retained unknown fact");
                };
                assert!(raw_payload.is_empty());
                batch.facts()[0].id
            })
            .collect::<Vec<_>>();
        let first_semantic_revisions = first_decoded
            .items
            .iter()
            .map(|item| {
                let ScopedDecodedAppendItem::Record { batch, .. } = item else {
                    panic!("expected decoded record");
                };
                batch.facts()[0]
                    .semantic_revision
                    .expect("fixture adapter uses canonical derived emission")
            })
            .collect::<Vec<_>>();
        assert!(object.checkpoint().is_none());
        assert!(object.decoder_state().is_none());
        object.discard(&first).unwrap();
        drop(first_pass);

        let replay_pass = host.begin_pass().unwrap();
        let replay = object.reconcile(&replay_pass, request()).unwrap();
        let mut lane = admission_lane(16, 0, 4);
        let mismatch = lane
            .admit(&mut object, &replay, first_decoded)
            .expect_err("old decoded receipt must not admit a replay observation");
        assert_eq!(mismatch.error, ScopedAdmissionError::ObservationMismatch);
        assert!(lane.is_empty());
        let ScopedAppendDecodeOutcome::Ready(replay_decoded) =
            decode_scoped(&host, &mut object, &replay)
        else {
            panic!("expected replay decode");
        };
        let replay_fact_ids = replay_decoded
            .items
            .iter()
            .map(|item| {
                let ScopedDecodedAppendItem::Record { batch, .. } = item else {
                    panic!("expected decoded replay record");
                };
                batch.facts()[0].id
            })
            .collect::<Vec<_>>();
        let replay_semantic_revisions = replay_decoded
            .items
            .iter()
            .map(|item| {
                let ScopedDecodedAppendItem::Record { batch, .. } = item else {
                    panic!("expected decoded replay record");
                };
                batch.facts()[0]
                    .semantic_revision
                    .expect("fixture adapter uses canonical derived emission")
            })
            .collect::<Vec<_>>();
        assert_eq!(replay_fact_ids, first_fact_ids);
        assert_eq!(replay_semantic_revisions, first_semantic_revisions);
        assert!(object.decoder_state().is_none());
        let mut full_lane = admission_lane(1, 0, 1);
        let backpressure = full_lane
            .admit(&mut object, &replay, replay_decoded)
            .expect_err("two decoded events must not enter one event slot");
        assert_eq!(backpressure.error, ScopedAdmissionError::DataQueueFull);
        assert!(full_lane.is_empty());
        assert!(object.checkpoint().is_none());
        assert!(object.decoder_state().is_none());
        let replay_decoded = backpressure.decoded;
        let replay_receipt = match lane.admit(&mut object, &replay, replay_decoded) {
            Ok(receipt) => receipt,
            Err(failure) => panic!("replay admission failed: {}", failure.error),
        };
        assert_eq!(replay_receipt.data_events, 2);
        assert_eq!(replay_receipt.control_items, 0);
        assert_eq!(lane.queued_data_events(), 2);
        assert!(matches!(
            lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Decoded {
                lane_ordinal: 1,
                ..
            })
        ));
        assert!(matches!(
            lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Decoded {
                lane_ordinal: 2,
                ..
            })
        ));
        assert!(lane.is_empty());
        assert_eq!(object.checkpoint().unwrap().committed_offset, 8);
        assert_eq!(object.decoder_state(), Some(b"onetwo".as_slice()));
        drop(replay_pass);

        let mut append = OpenOptions::new()
            .append(true)
            .open(root.join("session.jsonl"))
            .unwrap();
        append.write_all(b"retry\n").unwrap();
        append.flush().unwrap();
        let retry_pass = host.begin_pass().unwrap();
        let retry = object.reconcile(&retry_pass, request()).unwrap();
        assert!(matches!(
            decode_scoped(&host, &mut object, &retry),
            ScopedAppendDecodeOutcome::RetryTransient
        ));
        assert_eq!(object.checkpoint().unwrap().committed_offset, 8);
        assert_eq!(object.decoder_state(), Some(b"onetwo".as_slice()));
        object.discard(&retry).unwrap();
        drop(retry_pass);

        std::fs::write(root.join("session.jsonl"), b"fresh\n").unwrap();
        let correction_pass = host.begin_pass().unwrap();
        let correction = object.reconcile(&correction_pass, request()).unwrap();
        assert_eq!(correction.phase, ScopedAppendDeliveryPhase::Correction);
        let ScopedAppendDecodeOutcome::Ready(correction_decoded) =
            decode_scoped(&host, &mut object, &correction)
        else {
            panic!("expected correction decode");
        };
        assert_eq!(correction_decoded.items.len(), 1);
        assert_eq!(object.decoder_state(), Some(b"onetwo".as_slice()));
        let correction_receipt = match lane.admit(&mut object, &correction, correction_decoded) {
            Ok(receipt) => receipt,
            Err(failure) => panic!("correction admission failed: {}", failure.error),
        };
        assert_eq!(correction_receipt.control_items, 1);
        assert_eq!(correction_receipt.through_lane_ordinal, 4);
        assert!(matches!(
            lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Reset {
                lane_ordinal: 3,
                reset: crate::scoped_observation::ScopedAppendReset {
                    reason: AppendTransition::Truncated,
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Decoded {
                lane_ordinal: 4,
                ..
            })
        ));
        assert_eq!(object.checkpoint().unwrap().generation, 2);
        assert_eq!(object.decoder_state(), Some(b"fresh".as_slice()));
    }

    #[test]
    fn scoped_admission_lane_bounds_actual_retained_native_bytes() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("retained-native-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 64;
        config.max_batch_bytes = 64;
        config.max_records_per_batch = 4;
        config.prefix_anchor_bytes = 16;
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(config).unwrap(),
            RawRetentionPolicy::Full,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"retained-native-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let pass = host.begin_pass().unwrap();
        let observation = object
            .reconcile(
                &pass,
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    access_phase: AccessPhase::Initial,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin: &origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();
        let ScopedAppendDecodeOutcome::Ready(decoded) =
            decode_scoped(&host, &mut object, &observation)
        else {
            panic!("expected decoded append batch");
        };

        let mut too_small = admission_lane(1, 5, 1);
        let backpressure = too_small
            .admit(&mut object, &observation, decoded)
            .expect_err("two retained copies of the three-byte payload require six bytes");
        assert_eq!(
            backpressure.error,
            ScopedAdmissionError::RetainedNativeQueueFull
        );
        assert_eq!(too_small.queued_retained_native_bytes(), 0);
        assert!(object.checkpoint().is_none());
        assert!(object.decoder_state().is_none());

        let mut exact = admission_lane(1, 6, 1);
        let receipt = match exact.admit(&mut object, &observation, backpressure.decoded) {
            Ok(receipt) => receipt,
            Err(failure) => panic!("exact retained-byte admission failed: {}", failure.error),
        };
        assert_eq!(receipt.retained_native_bytes, 6);
        assert_eq!(exact.queued_retained_native_bytes(), 6);
        assert!(matches!(
            exact.pop_next(),
            Some(ScopedQueuedObservationFrame::Decoded { .. })
        ));
        assert_eq!(exact.queued_retained_native_bytes(), 0);
        assert_eq!(object.checkpoint().unwrap().committed_offset, 4);
        assert_eq!(object.decoder_state(), Some(b"one".as_slice()));
    }

    #[test]
    fn scoped_decode_dependency_access_fails_closed_without_a_declared_relation() {
        let registry = dependency_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("dependency-denied-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 64;
        config.max_batch_bytes = 64;
        config.max_records_per_batch = 4;
        config.prefix_anchor_bytes = 16;
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(config).unwrap(),
            RawRetentionPolicy::None,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"dependency-denied-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let pass = host.begin_pass().unwrap();
        let observation = object
            .reconcile(
                &pass,
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    access_phase: AccessPhase::Initial,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin: &origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();

        assert!(matches!(
            host.decode_append(&mut object, &observation),
            Err(ScopedObservationAccessError::Decode(
                ScopedDecodeFailureClass::InvalidContract
            ))
        ));
        assert!(object.checkpoint().is_none());
        assert!(object.decoder_state().is_none());
        object.discard(&observation).unwrap();
    }

    #[test]
    fn scoped_append_bootstrap_barrier_waits_for_bounded_batch_drain() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("bounded-bootstrap-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"one\ntwo\n").unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 16;
        config.max_batch_bytes = 16;
        config.max_records_per_batch = 1;
        config.prefix_anchor_bytes = 16;
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(config).unwrap(),
            RawRetentionPolicy::None,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"bounded-bootstrap-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let request = || ScopedAppendReconcileRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            access_phase: AccessPhase::Initial,
            parent_token: None,
            depth: 1,
            max_bytes: 64,
            origin: &origin,
            force_contract_replay: false,
        };
        let source = object.source_identity().clone();
        let mut lane = admission_lane(4, 0, 1);
        let mut projection =
            ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 1,
            })
            .unwrap();
        let mut delivery = ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();

        let first_pass = host.begin_pass().unwrap();
        let first = object.reconcile(&first_pass, request()).unwrap();
        assert_eq!(first.phase, ScopedAppendDeliveryPhase::Bootstrap);
        assert!(matches!(
            &first.read,
            AppendRead::Batch {
                more_available: true,
                ..
            }
        ));
        assert!(matches!(
            object.complete_bootstrap(),
            Err(ScopedObservationAccessError::BootstrapNotDrained)
        ));
        let ScopedAppendDecodeOutcome::Ready(first_decoded) =
            decode_scoped(&host, &mut object, &first)
        else {
            panic!("expected first bounded bootstrap decode");
        };
        if let Err(failure) = lane.admit(&mut object, &first, first_decoded) {
            panic!("first bounded admission failed: {}", failure.error);
        }
        let first_offer = lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(first_offer.first_offered_sequence, None);
        assert_eq!(first_offer.offered_through_sequence, 0);
        let partial = lane.offered_decode_coverage(&source).unwrap();
        assert_eq!(partial.completeness, CoverageSetCompleteness::Partial);
        assert_eq!(
            partial.point.as_ref().unwrap().status,
            CoverageStatus::Partial
        );
        assert_eq!(
            partial
                .point
                .as_ref()
                .unwrap()
                .position
                .as_ref()
                .unwrap()
                .monotonic_order,
            Some(4)
        );
        assert_eq!(partial.explicit_errors.len(), 1);
        assert_eq!(partial.explicit_errors[0].code, "bounded_backlog");
        let partial_watermark = host
            .capture_watermark_core(&lane, &projection, &delivery)
            .unwrap();
        assert_eq!(partial_watermark.offered_through_sequence, 0);
        assert_eq!(partial_watermark.source_coverage.len(), 1);
        assert_eq!(
            partial_watermark.source_coverage[0].completeness,
            CoverageSetCompleteness::Partial
        );
        assert_eq!(partial_watermark.explicit_object_errors.len(), 1);
        assert_eq!(
            partial_watermark.explicit_object_errors[0].code,
            "bounded_backlog"
        );
        drop(first_pass);
        assert!(matches!(
            object.complete_bootstrap(),
            Err(ScopedObservationAccessError::BootstrapNotDrained)
        ));

        let second_pass = host.begin_pass().unwrap();
        let second = object.reconcile(&second_pass, request()).unwrap();
        assert_eq!(second.phase, ScopedAppendDeliveryPhase::Bootstrap);
        assert!(matches!(
            &second.read,
            AppendRead::Batch {
                more_available: false,
                ..
            }
        ));
        let ScopedAppendDecodeOutcome::Ready(second_decoded) =
            decode_scoped(&host, &mut object, &second)
        else {
            panic!("expected second bounded bootstrap decode");
        };
        if let Err(failure) = lane.admit(&mut object, &second, second_decoded) {
            panic!("second bounded admission failed: {}", failure.error);
        }
        let second_offer = lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(second_offer.first_offered_sequence, None);
        assert_eq!(second_offer.offered_through_sequence, 0);
        let complete = lane.offered_decode_coverage(&source).unwrap();
        assert_eq!(complete.completeness, CoverageSetCompleteness::Complete);
        assert_eq!(
            complete.point.as_ref().unwrap().status,
            CoverageStatus::CompleteThrough
        );
        assert_eq!(
            complete
                .point
                .as_ref()
                .unwrap()
                .position
                .as_ref()
                .unwrap()
                .monotonic_order,
            Some(8)
        );
        assert!(complete.explicit_errors.is_empty());
        let complete_watermark = host
            .capture_watermark_core(&lane, &projection, &delivery)
            .unwrap();
        assert_eq!(complete_watermark.offered_through_sequence, 0);
        assert_eq!(complete_watermark.source_coverage.len(), 1);
        assert!(complete_watermark.explicit_object_errors.is_empty());
        drop(second_pass);
        object.complete_bootstrap().unwrap();
        assert!(!object.bootstrap_active());
        assert_eq!(object.checkpoint().unwrap().committed_offset, 8);
    }

    #[test]
    fn scoped_bootstrap_barrier_is_ordered_idempotent_and_replay_stable() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("bootstrap-barrier-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"bootstrap-barrier-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let pass = host.begin_pass().unwrap();
        let observation = object
            .reconcile(
                &pass,
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    access_phase: AccessPhase::Initial,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin: &origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();
        assert!(!observation.object_present);
        let ScopedAppendDecodeOutcome::Ready(decoded) =
            decode_scoped(&host, &mut object, &observation)
        else {
            panic!("missing root should decode to an empty bootstrap batch");
        };
        let mut admission = admission_lane(1, 0, 1);
        if let Err(failure) = admission.admit(&mut object, &observation, decoded) {
            panic!("missing-root admission failed: {}", failure.error);
        }
        drop(pass);
        object.complete_bootstrap().unwrap();

        let mut projection =
            ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 1,
            })
            .unwrap();
        let mut delivery = ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
        let barrier = host
            .offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut delivery,
                50,
            )
            .unwrap();
        assert_eq!(barrier.scope_epoch, 1);
        assert_eq!(barrier.barrier_sequence, 1);
        assert!(!barrier.root_present);
        assert_eq!(barrier.source_coverage.len(), 2);
        assert_eq!(barrier.family_manifest.len(), 1);
        assert_eq!(barrier.family_manifest[0].fact_family, "runtime.usage-v2");
        assert_eq!(barrier.family_manifest[0].entity_or_event_count, 0);
        assert!(barrier.explicit_object_errors.is_empty());
        assert_eq!(barrier.queue_state.offered_through_sequence, 1);
        assert_eq!(barrier.queue_state.queued_source_control_items, 1);
        assert_eq!(delivery.state(), barrier.queue_state);

        let repeated = host
            .offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut delivery,
                999,
            )
            .unwrap();
        assert!(Arc::ptr_eq(&barrier, &repeated));
        assert_eq!(delivery.queued_source_control_items(), 1);

        let other_root = temp.path().join("other-bootstrap-barrier-root");
        std::fs::create_dir_all(&other_root).unwrap();
        let mut other_request = scoped_access_request(other_root);
        other_request.root_identity = ScopedRootIdentityRequest::new(
            1,
            b"fixture-other-source-instance".as_slice(),
            b"fixture-other-session".as_slice(),
            None,
            None,
            None,
        );
        let other_host = ScopedObservationAccessHost::authorize(&registry, other_request).unwrap();
        assert!(matches!(
            other_host.offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut delivery,
                50,
            ),
            Err(ScopedBootstrapBarrierError::StateChanged)
        ));

        let delivered = delivery.pop_next().unwrap();
        let first_event_id = delivered.event_id;
        let envelope = host.envelope_mapper().map(delivered).unwrap();
        assert_eq!(envelope.observer_sequence, barrier.barrier_sequence);
        assert_eq!(envelope.semantic_revision_ref, None);
        assert_eq!(envelope.observed_at, 50);
        assert_eq!(
            envelope.actor.run_key,
            host.root_identity().root_actor_run_key
        );
        assert_eq!(
            envelope.actor_attribution,
            ScopedActorAttribution::ScopeFallback {
                reason: ScopedActorFallbackReason::ObserverLifecycleControl,
            }
        );
        assert_eq!(
            envelope.evidence.authority,
            ScopedEnvelopeEvidenceAuthority::EngineControl
        );
        assert!(matches!(
            envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete {
                barrier: delivered_barrier,
            } if Arc::ptr_eq(&barrier, &delivered_barrier)
        ));
        assert!(delivery.is_empty());
        assert!(Arc::ptr_eq(
            &barrier,
            &delivery.bootstrap_barrier().unwrap()
        ));
        assert_eq!(
            other_host.require_resync(&mut delivery, ScopedResyncReason::WatcherOverflow, 60,),
            Err(ScopedContinuityError::RootMismatch)
        );
        let resync = host
            .require_resync(&mut delivery, ScopedResyncReason::WatcherOverflow, 60)
            .unwrap();
        assert_eq!(resync.last_contiguous_sequence, 1);
        assert_eq!(
            other_host.begin_resync(&mut delivery, 70),
            Err(ScopedContinuityError::RootMismatch)
        );
        assert_eq!(
            host.begin_resync(&mut delivery, 70),
            Err(ScopedContinuityError::ResyncRequiredNotDelivered)
        );
        assert!(matches!(
            host.capture_watermark_core(&admission, &projection, &delivery),
            Err(ScopedCoverageAssemblyError::ContinuityInvalid)
        ));
        assert!(matches!(
            host.offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut delivery,
                60,
            ),
            Err(ScopedBootstrapBarrierError::StateChanged)
        ));

        let required_delivery = delivery.pop_next().unwrap();
        assert_eq!(required_delivery.observer_sequence, 2);
        assert!(matches!(
            host.envelope_mapper().map(required_delivery).unwrap().event,
            ScopedObservationEvent::ObserverResyncRequired { .. }
        ));
        let started = host.begin_resync(&mut delivery, 70).unwrap();
        assert_eq!(started.old_scope_epoch, 1);
        assert_eq!(started.new_scope_epoch, 2);
        let mut stage = host.open_resync_stage(&projection, &delivery).unwrap();
        let replacement = stage.prepare_snapshot(&admission, &delivery).unwrap();
        assert_eq!(replacement.entity_count, 0);
        assert!(replacement.events.is_empty());
        let replacement_semantic_digest = replacement.semantic_digest;
        assert!(stage.snapshot_fully_offered());

        assert_eq!(
            host.offer_resync_complete(
                &mut projection,
                &mut stage,
                &admission,
                &mut delivery,
                false,
                80,
            ),
            Err(ScopedReplacementStageError::Delivery(
                ScopedDeliveryError::SourceControlQueueFull
            ))
        );
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Resyncing
        );
        assert!(delivery.resync_barrier().is_none());

        let started_delivery = delivery.pop_next().unwrap();
        assert_eq!(started_delivery.observer_sequence, 3);
        assert!(matches!(
            host.envelope_mapper().map(started_delivery).unwrap().event,
            ScopedObservationEvent::ObserverResyncStarted { .. }
        ));
        let resync_barrier = host
            .offer_resync_complete(
                &mut projection,
                &mut stage,
                &admission,
                &mut delivery,
                false,
                80,
            )
            .unwrap();
        assert_eq!(resync_barrier.scope_epoch, 2);
        assert_eq!(resync_barrier.started_control_sequence, 3);
        assert_eq!(resync_barrier.barrier_sequence, 4);
        assert_eq!(
            resync_barrier.replacement,
            ScopedReplacementMode::FullSnapshot
        );
        assert_eq!(resync_barrier.family_manifest.len(), 1);
        assert_eq!(
            resync_barrier.family_manifest[0].fact_family,
            "runtime.usage-v2"
        );
        assert_eq!(resync_barrier.family_manifest[0].contract_version, 1);
        assert_eq!(
            resync_barrier.family_manifest[0].replacement_representation,
            ScopedReplacementRepresentation::UsageLatestContributionPerResponse
        );
        assert_eq!(
            resync_barrier.family_manifest[0].completeness,
            CoverageSetCompleteness::Complete
        );
        assert_eq!(resync_barrier.family_manifest[0].entity_or_event_count, 0);
        assert_eq!(
            resync_barrier.family_manifest[0].semantic_digest,
            replacement_semantic_digest
        );
        assert_eq!(resync_barrier.source_coverage.len(), 2);
        assert_eq!(resync_barrier.family_manifest, barrier.family_manifest);
        assert_eq!(
            resync_barrier.replacement_snapshot_digest,
            barrier.replacement_snapshot_digest
        );
        assert!(!resync_barrier.root_present);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Valid
        );
        assert!(delivery.resync_required().is_none());
        assert!(delivery.resync_started().is_none());
        assert!(Arc::ptr_eq(
            &resync_barrier,
            &delivery.resync_barrier().unwrap()
        ));
        let repeated_complete = host
            .offer_resync_complete(
                &mut projection,
                &mut stage,
                &admission,
                &mut delivery,
                true,
                999,
            )
            .unwrap();
        assert!(Arc::ptr_eq(&resync_barrier, &repeated_complete));
        assert_eq!(delivery.queued_source_control_items(), 1);

        let completed_delivery = delivery.pop_next().unwrap();
        let completed_event_id = completed_delivery.event_id;
        assert_eq!(completed_delivery.observer_sequence, 4);
        let completed_envelope = host.envelope_mapper().map(completed_delivery).unwrap();
        assert_eq!(completed_envelope.scope_epoch, 2);
        assert_eq!(completed_envelope.observed_at, 80);
        assert!(matches!(
            completed_envelope.event,
            ScopedObservationEvent::ObserverResyncComplete {
                barrier: delivered_barrier,
            } if Arc::ptr_eq(&resync_barrier, &delivered_barrier)
        ));
        assert!(delivery.is_empty());

        let next_resync = host
            .require_resync(
                &mut delivery,
                ScopedResyncReason::ExplicitConsumerRequest,
                90,
            )
            .unwrap();
        assert_eq!(next_resync.invalid_scope_epoch, 2);
        assert_eq!(
            next_resync.baseline_snapshot_digest,
            resync_barrier.replacement_snapshot_digest
        );
        assert_ne!(completed_event_id, first_event_id);

        // A second attachment-local lane replays the same bootstrap snapshot
        // under a different observation time without changing control ID.
        let mut replay_delivery =
            ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        let replay_barrier = host
            .offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut replay_delivery,
                5_000,
            )
            .unwrap();
        assert_eq!(replay_barrier.snapshot_digest, barrier.snapshot_digest);
        assert_eq!(replay_delivery.pop_next().unwrap().event_id, first_event_id);
    }

    #[test]
    fn scoped_whole_epoch_completion_swaps_source_coverage_and_reducer_state() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("whole-epoch-replacement-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"whole-epoch-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let request = || ScopedAppendReconcileRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            access_phase: AccessPhase::Initial,
            parent_token: None,
            depth: 1,
            max_bytes: 128,
            origin: &origin,
            force_contract_replay: false,
        };

        let bootstrap_pass = host.begin_pass().unwrap();
        let bootstrap_observation = object.reconcile(&bootstrap_pass, request()).unwrap();
        assert!(!bootstrap_observation.object_present);
        let ScopedAppendDecodeOutcome::Ready(bootstrap_decoded) =
            decode_scoped(&host, &mut object, &bootstrap_observation)
        else {
            panic!("missing bootstrap root should decode to an empty batch");
        };
        let mut admission = admission_lane(2, 0, 2);
        if let Err(failure) =
            admission.admit(&mut object, &bootstrap_observation, bootstrap_decoded)
        {
            panic!("bootstrap admission failed: {}", failure.error);
        }
        drop(bootstrap_pass);
        object.complete_bootstrap().unwrap();
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let mut delivery = ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
        let bootstrap_barrier = host
            .offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut delivery,
                50,
            )
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);
        let mut active = host
            .bind_bootstrap_epoch_state(vec![object], admission, projection, &delivery)
            .unwrap();
        assert_eq!(active.scope_epoch(), 1);
        assert!(!active.append_object("root-object").unwrap().is_present());

        // Commit one live read into the bounded admission lane but do not
        // offer it. Continuity loss must supersede this old-epoch backlog,
        // even though admission has already advanced the source cursor.
        std::fs::write(root.join("session.jsonl"), b"old-unoffered\n").unwrap();
        let live_pass = host.begin_pass().unwrap();
        let live_observation = {
            let (active_object, _) = active
                .append_object_and_admission_mut("root-object")
                .unwrap();
            active_object.reconcile(&live_pass, request()).unwrap()
        };
        let live_decoded = {
            let (active_object, _) = active
                .append_object_and_admission_mut("root-object")
                .unwrap();
            let ScopedAppendDecodeOutcome::Ready(decoded) =
                decode_scoped(&host, active_object, &live_observation)
            else {
                panic!("live root should decode one bounded batch");
            };
            decoded
        };
        {
            let (active_object, active_admission) = active
                .append_object_and_admission_mut("root-object")
                .unwrap();
            if let Err(failure) =
                active_admission.admit(active_object, &live_observation, live_decoded)
            {
                panic!("live admission failed: {}", failure.error);
            }
        }
        drop(live_pass);
        assert!(!active.admission_is_empty());
        assert_eq!(
            active
                .append_object("root-object")
                .unwrap()
                .checkpoint()
                .unwrap()
                .committed_offset,
            14
        );

        host.require_resync(&mut delivery, ScopedResyncReason::WatcherOverflow, 60)
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 2);
        host.begin_resync(&mut delivery, 70).unwrap();
        let mut stage = host
            .open_scope_resync_stage(&mut active, &delivery)
            .unwrap();
        assert_eq!(stage.scope_epoch(), 2);
        assert_eq!(
            active
                .append_object("root-object")
                .unwrap()
                .frozen_scope_epoch(),
            Some(2)
        );

        std::fs::write(root.join("session.jsonl"), b"replacement\n").unwrap();
        let replacement_pass = host.begin_pass().unwrap();
        let replacement_observation = {
            let (replacement_object, _) = stage
                .append_object_and_admission_mut("root-object", &delivery)
                .unwrap();
            replacement_object
                .reconcile(&replacement_pass, request())
                .unwrap()
        };
        assert_eq!(
            replacement_observation.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert!(replacement_observation.object_present);
        let replacement_decoded = {
            let (replacement_object, _) = stage
                .append_object_and_admission_mut("root-object", &delivery)
                .unwrap();
            let ScopedAppendDecodeOutcome::Ready(decoded) =
                decode_scoped(&host, replacement_object, &replacement_observation)
            else {
                panic!("replacement root should decode one bounded batch");
            };
            decoded
        };
        {
            let (replacement_object, replacement_admission) = stage
                .append_object_and_admission_mut("root-object", &delivery)
                .unwrap();
            if let Err(failure) = replacement_admission.admit(
                replacement_object,
                &replacement_observation,
                replacement_decoded,
            ) {
                panic!("replacement admission failed: {}", failure.error);
            }
        }
        drop(replacement_pass);
        assert!(stage.reduce_next(&delivery).unwrap());
        assert!(!stage.reduce_next(&delivery).unwrap());
        let replacement = stage.prepare_snapshot(&delivery).unwrap();
        assert_eq!(replacement.entity_count, 0);
        assert!(replacement.events.is_empty());
        assert!(stage.snapshot_fully_offered());

        // observer.resync_started still owns the one control slot. Completion
        // failure cannot activate any of the three staged state components.
        assert_eq!(
            host.offer_scope_resync_complete(&mut active, &mut stage, &mut delivery, 80,),
            Err(ScopedReplacementStageError::Delivery(
                ScopedDeliveryError::SourceControlQueueFull
            ))
        );
        assert_eq!(active.scope_epoch(), 1);
        assert_eq!(
            active
                .append_object("root-object")
                .unwrap()
                .frozen_scope_epoch(),
            Some(2)
        );
        assert!(!active.admission_is_empty());
        assert!(stage.admission_is_empty());
        assert!(!stage.is_activated());

        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 3);
        let resync_barrier = host
            .offer_scope_resync_complete(&mut active, &mut stage, &mut delivery, 80)
            .unwrap();
        assert!(resync_barrier.root_present);
        assert_eq!(active.scope_epoch(), 2);
        let active_object = active.append_object("root-object").unwrap();
        assert!(active_object.is_present());
        assert_eq!(active_object.checkpoint().unwrap().committed_offset, 12);
        assert_eq!(active_object.frozen_scope_epoch(), None);
        assert_eq!(active_object.replacement_scope_epoch(), None);
        assert!(!active_object.is_retired());
        let retired_object = stage.append_object("root-object").unwrap();
        assert!(retired_object.is_present());
        assert_eq!(retired_object.checkpoint().unwrap().committed_offset, 14);
        assert!(retired_object.is_retired());
        assert!(stage.is_activated());
        assert!(active.admission_is_empty());
        assert!(stage.admission_is_empty());

        let active_watermark = host.capture_epoch_watermark(&active, &delivery).unwrap();
        assert_eq!(active_watermark.scope_epoch, 2);
        assert_eq!(
            active_watermark.source_coverage,
            resync_barrier.source_coverage
        );
        assert_ne!(
            resync_barrier.replacement_snapshot_digest,
            bootstrap_barrier.replacement_snapshot_digest
        );
        let repeated = host
            .offer_scope_resync_complete(&mut active, &mut stage, &mut delivery, 999)
            .unwrap();
        assert!(Arc::ptr_eq(&resync_barrier, &repeated));
        assert_eq!(delivery.queued_source_control_items(), 1);

        // A later re-overflow abandons the whole stage while retaining the
        // last activated source state. The next epoch supersedes the frozen
        // lineage, so the stale stage can never activate.
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 4);
        host.require_resync(
            &mut delivery,
            ScopedResyncReason::ExplicitConsumerRequest,
            90,
        )
        .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 5);
        host.begin_resync(&mut delivery, 100).unwrap();
        let mut stale_stage = host
            .open_scope_resync_stage(&mut active, &delivery)
            .unwrap();
        assert_eq!(active.scope_epoch(), 2);
        assert_eq!(
            active
                .append_object("root-object")
                .unwrap()
                .frozen_scope_epoch(),
            Some(3)
        );
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 6);
        host.require_resync(
            &mut delivery,
            ScopedResyncReason::TransportContinuityLoss,
            110,
        )
        .unwrap();
        assert!(matches!(
            stale_stage.append_object_and_admission_mut("root-object", &delivery),
            Err(ScopedReplacementStageError::NotResyncing)
        ));
        assert_eq!(active.scope_epoch(), 2);
        assert_eq!(
            active
                .append_object("root-object")
                .unwrap()
                .checkpoint()
                .unwrap()
                .committed_offset,
            12
        );
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 7);
        host.begin_resync(&mut delivery, 120).unwrap();
        let fresh_stage = host
            .open_scope_resync_stage(&mut active, &delivery)
            .unwrap();
        assert_eq!(fresh_stage.scope_epoch(), 4);
        assert_eq!(
            active
                .append_object("root-object")
                .unwrap()
                .frozen_scope_epoch(),
            Some(4)
        );
        assert!(matches!(
            stale_stage.append_object_and_admission_mut("root-object", &delivery),
            Err(ScopedReplacementStageError::EpochMismatch)
        ));
        assert_eq!(
            stale_stage
                .append_object("root-object")
                .unwrap()
                .replacement_scope_epoch(),
            Some(3)
        );
        assert_eq!(
            fresh_stage
                .append_object("root-object")
                .unwrap()
                .replacement_scope_epoch(),
            Some(4)
        );
    }

    #[test]
    fn scoped_bootstrap_barrier_waits_for_admission_drain() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("bootstrap-pending-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"bootstrap-pending-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let pass = host.begin_pass().unwrap();
        let observation = object
            .reconcile(
                &pass,
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    access_phase: AccessPhase::Initial,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin: &origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();
        let ScopedAppendDecodeOutcome::Ready(decoded) =
            decode_scoped(&host, &mut object, &observation)
        else {
            panic!("present root should decode one bootstrap batch");
        };
        let mut admission = admission_lane(1, 0, 1);
        if let Err(failure) = admission.admit(&mut object, &observation, decoded) {
            panic!("bootstrap admission failed: {}", failure.error);
        }
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let mut delivery = ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
        assert!(matches!(
            host.offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut delivery,
                50,
            ),
            Err(ScopedBootstrapBarrierError::Coverage(
                ScopedCoverageAssemblyError::AdmissionNotDrained
            ))
        ));
        assert!(delivery.bootstrap_barrier().is_none());
        assert!(delivery.is_empty());
    }
}
