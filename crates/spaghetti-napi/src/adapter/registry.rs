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

    use tempfile::TempDir;

    use crate::adapter::{
        verify_support_release_bundle, AdapterManifest, AdapterObjectContext,
        AdapterSupportBinding, CoverageAbsenceKind, CoverageDomain, CoveragePositionKind,
        CoverageSetCompleteness, CoverageStatus, DecodeContext, DecodeDisposition, DecoderId,
        DiscoveryContext, Fact, FactBatch, FactSemanticContext, RawRetentionPolicy, Sha256Digest,
        SourceAccess, SourceInstance, SourceInstanceSpec, SourceObjectDescriptor, StreamSpec,
        SupportBundleDocument,
    };
    use crate::scoped_observation::{
        ScopedAdmissionError, ScopedAppendDecodeOutcome, ScopedAppendDecoderConfig,
        ScopedAppendDeliveryPhase, ScopedAppendObservation, ScopedAppendPresenceChange,
        ScopedAppendReconcileRequest, ScopedCoverageAssemblyError, ScopedDecodeFailureClass,
        ScopedDecodedAppendItem, ScopedDeliveryError, ScopedKnownAppendObject,
        ScopedKnownObjectGrant, ScopedKnownObjectReadRequest, ScopedObjectRead,
        ScopedObservationAccessError, ScopedObservationAccessHost, ScopedObservationAccessRequest,
        ScopedObservationAdmissionLane, ScopedObservationDeliveryLane,
        ScopedObservationDeliveryLimits, ScopedObservationProjectionLimits,
        ScopedObservationProjectionSink, ScopedObservationQueueLimits,
        ScopedProjectionDeliveryError, ScopedQueuedObservationFrame, ScopedSourceFailureClass,
    };
    use crate::source::{
        AccessOperation, AccessOutcome, AccessPhase, AppendDelimitedConfig, AppendDelimitedFile,
        AppendItem, AppendRead, AppendTransition, AuthorizedScopeAccessPlan, RecordOrigin,
        ScopeAccessReport, ScopeAccessRequest, ScopeIdentityInput, SourceMediaType, SourceRecord,
    };

    use super::*;

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
            program_id: "observe-session".to_string(),
            known_objects: vec![ScopedKnownObjectGrant {
                relation_id: "root-object".to_string(),
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
        FactSemanticContext::new(
            &AdapterId::new("fixture").unwrap(),
            1,
            b"fixture-source-instance",
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
        assert_eq!(missing.phase, ScopedAppendDeliveryPhase::Bootstrap);
        assert!(!missing.root_present);
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
        assert!(object.root_present());
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
        assert!(!object.root_present());
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
        assert!(object.root_present());
        assert_eq!(object.checkpoint().unwrap().generation, 3);
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
}
