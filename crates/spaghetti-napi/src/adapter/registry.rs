use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;

use super::{
    AdapterError, AdapterId, AdapterSupportRegistration, AgentAdapter, CompatibilityDecision,
    ContractVersionOffer, ContractVersionRequest, NativeArtifactProbe, SupportCatalog,
    SupportOperation, TypedAccessAuthorization,
};

type NativeSupportProbeDriver =
    dyn Fn(&[PathBuf]) -> Result<NativeArtifactProbe, AdapterError> + Send + Sync + 'static;

pub struct AdapterRegistryBuilder {
    adapters: Vec<Arc<dyn AgentAdapter>>,
    native_support_probe_drivers: Vec<(String, Arc<NativeSupportProbeDriver>)>,
}

impl AdapterRegistryBuilder {
    pub fn new() -> Self {
        Self {
            adapters: Vec::new(),
            native_support_probe_drivers: Vec::new(),
        }
    }

    pub fn register<A>(mut self, adapter: A) -> Self
    where
        A: AgentAdapter,
    {
        self.adapters.push(Arc::new(adapter));
        self
    }

    /// Register one trusted-host probe driver without granting the adapter
    /// filesystem authority. The driver is resolved by adapter ID only after
    /// the adapter registry and support catalog have both been verified.
    pub(crate) fn register_native_support_probe<F>(mut self, adapter_id: &str, driver: F) -> Self
    where
        F: Fn(&[PathBuf]) -> Result<NativeArtifactProbe, AdapterError> + Send + Sync + 'static,
    {
        self.native_support_probe_drivers
            .push((adapter_id.to_string(), Arc::new(driver)));
        self
    }

    pub fn build(self) -> Result<AdapterRegistry, AdapterError> {
        self.build_inner(None, false)
    }

    /// Explicit compatibility path for hosts that predate promoted RFC 012A
    /// support packages. This registry cannot mint typed-access authority.
    pub fn build_legacy(self) -> Result<AdapterRegistry, AdapterError> {
        self.build_inner(None, false)
    }

    /// Verify every registered adapter against a compiled support bundle while
    /// retaining candidate adapters on the explicit non-authorizing legacy
    /// path. Typed access still selects promoted releases only.
    pub(crate) fn build_verified(
        self,
        support_catalog: Arc<SupportCatalog>,
    ) -> Result<AdapterRegistry, AdapterError> {
        self.build_inner(Some(support_catalog), false)
    }

    pub fn build_supported(
        self,
        support_catalog: Arc<SupportCatalog>,
    ) -> Result<AdapterRegistry, AdapterError> {
        self.build_inner(Some(support_catalog), true)
    }

    fn build_inner(
        self,
        support_catalog: Option<Arc<SupportCatalog>>,
        require_promoted_registrations: bool,
    ) -> Result<AdapterRegistry, AdapterError> {
        let Self {
            adapters: registered_adapters,
            native_support_probe_drivers: registered_probe_drivers,
        } = self;
        let mut adapters = BTreeMap::new();
        for adapter in registered_adapters {
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
                        require_promoted_registrations,
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
        let mut native_support_probe_drivers = BTreeMap::new();
        for (raw_adapter_id, driver) in registered_probe_drivers {
            let adapter_id = AdapterId::new(raw_adapter_id)?;
            if !adapters.contains_key(&adapter_id) {
                return Err(AdapterError::invalid_contract(format!(
                    "native support probe references unregistered adapter {adapter_id}"
                )));
            }
            if native_support_probe_drivers
                .insert(adapter_id.clone(), driver)
                .is_some()
            {
                return Err(AdapterError::invalid_contract(format!(
                    "duplicate native support probe for adapter {adapter_id}"
                )));
            }
        }
        Ok(AdapterRegistry {
            adapters,
            native_support_probe_drivers,
            support_catalog,
            require_promoted_registrations,
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
    native_support_probe_drivers: BTreeMap<AdapterId, Arc<NativeSupportProbeDriver>>,
    support_catalog: Option<Arc<SupportCatalog>>,
    require_promoted_registrations: bool,
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
        self.require_promoted_registrations
    }

    pub(crate) fn has_verified_support_catalog(&self) -> bool {
        self.support_catalog.is_some()
    }

    /// Run a host-registered, bounded native probe under panic containment.
    /// The adapter never receives the roots or performs this artifact read.
    pub(crate) fn probe_native_support(
        &self,
        adapter_id: &AdapterId,
        roots: &[PathBuf],
    ) -> Result<Option<NativeArtifactProbe>, AdapterError> {
        if !self.adapters.contains_key(adapter_id) {
            return Err(AdapterError::invalid_contract(
                "native support probe references an unregistered adapter",
            ));
        }
        let Some(driver) = self.native_support_probe_drivers.get(adapter_id) else {
            return Ok(None);
        };
        catch_unwind(AssertUnwindSafe(|| driver(roots)))
            .map_err(|_| {
                AdapterError::new(
                    super::AdapterErrorClass::AdapterFatal,
                    "native_support_probe_panic",
                    "trusted host native support probe panicked",
                )
            })?
            .map(Some)
    }

    /// Select the promoted durable contract for a native artifact when the
    /// compiled catalog recognizes it. Recognized-but-unverified and
    /// candidate artifacts stay on the caller's explicit legacy path and do
    /// not receive a typed authorization.
    pub(crate) fn authorize_durable_if_supported(
        &self,
        adapter_id: &AdapterId,
        probe: &NativeArtifactProbe,
    ) -> Result<Option<TypedAccessAuthorization>, AdapterError> {
        self.authorize_supported_operation(
            adapter_id,
            probe,
            SupportOperation::DurableHistoryRuntime,
            &[
                "runtime.actor-run",
                "runtime.actor-affiliation",
                "runtime.usage-v2",
            ],
        )
    }

    /// Select a promoted scoped-observation contract for one already-probed
    /// native artifact. Candidate, recognized-unverified, and incompatible
    /// artifacts deliberately return `None`; they never reach the stricter
    /// typed-access constructor and cannot acquire source authority.
    ///
    /// The containing trusted host must negotiate the complete RFC 012D
    /// observation contract before it performs the native probe and calls
    /// this method. Keeping this selector in the registry prevents a future
    /// portable transport from treating a caller-supplied classification as
    /// authority.
    pub(crate) fn authorize_scoped_if_supported(
        &self,
        adapter_id: &AdapterId,
        probe: &NativeArtifactProbe,
        request: &ContractVersionRequest,
        offer: &ContractVersionOffer,
    ) -> Result<Option<(CompatibilityDecision, TypedAccessAuthorization)>, AdapterError> {
        let Some(catalog) = self.support_catalog.as_ref() else {
            return Ok(None);
        };
        let decision = catalog
            .classify(probe)
            .map_err(|error| AdapterError::invalid_contract(error.to_string()))?;
        if !decision.permissions().scoped_observation {
            return Ok(None);
        }
        self.authorize_typed_access(
            adapter_id,
            probe,
            SupportOperation::ScopedTypedObservation,
            request,
            offer,
        )
        .map(Some)
    }

    fn authorize_supported_operation(
        &self,
        adapter_id: &AdapterId,
        probe: &NativeArtifactProbe,
        operation: SupportOperation,
        fact_families: &[&str],
    ) -> Result<Option<TypedAccessAuthorization>, AdapterError> {
        let Some(catalog) = self.support_catalog.as_ref() else {
            return Ok(None);
        };
        let decision = catalog
            .classify(probe)
            .map_err(|error| AdapterError::invalid_contract(error.to_string()))?;
        let permitted = match operation {
            SupportOperation::CatalogDiscovery => decision.permissions().catalog,
            SupportOperation::DurableHistoryRuntime => decision.permissions().durable,
            _ => {
                return Err(AdapterError::invalid_contract(
                    "registry supported-operation selector received an unsupported operation",
                ));
            }
        };
        if !permitted {
            return Ok(None);
        }
        let mut fact_family_versions = BTreeMap::new();
        for family in fact_families {
            fact_family_versions.insert(family.to_string(), vec![1]);
        }
        let request = ContractVersionRequest {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: fact_family_versions.clone(),
            query_pack_versions: Some(vec![1]),
            observation_contract_versions: None,
        };
        let offer = ContractVersionOffer {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions,
            query_pack_versions: vec![1],
            observation_contract_versions: Vec::new(),
        };
        self.authorize_typed_access(adapter_id, probe, operation, &request, &offer)
            .map(|(_, authorization)| Some(authorization))
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
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use std::sync::Arc;

    use crate::adapter::{
        verify_support_release_bundle, AdapterManifest, AdapterObjectContext,
        AdapterSupportBinding, DecodeContext, DecodeDisposition, DiscoveryContext, FactBatch,
        NativeArtifactProbe, Sha256Digest, SourceInstance, SourceInstanceSpec,
        SourceObjectDescriptor, StreamSpec, SupportBundleDocument,
    };

    use crate::source::{
        AccessOperation, AccessOutcome, AccessPhase, AuthorizedScopeAccessPlan, ScopeAccessReport,
        ScopeAccessRequest, ScopeIdentityInput, SourceRecord,
    };

    use super::*;

    struct EmptyAdapter {
        manifest: AdapterManifest,
        streams: Vec<StreamSpec>,
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
                streams: Vec::new(),
            }
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

    const SINGLE_OBJECT_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"descendant-objects","primitive":"ChildDirectoryByNativeId","access_root":"root","locator":"sessions/{native-session-id}/children","identity_inputs":["native-session-id"],"directory_identity_authority":"configured_root","bounds":{"max_fan_out":8,"max_depth":2,"max_objects":8,"max_bytes":8192,"max_rows":0},"observation_binding":{"stream_id":"descendant-stream","source_pattern":"sessions/*/children/**","relative_selector":"**"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const JOIN_DERIVED_DYNAMIC_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"descendant-objects","primitive":"ChildDirectoryByNativeId","access_root":"root","locator":"sessions/{child-id}/children","identity_inputs":["child-id"],"directory_identity_authority":"scope_join","bounds":{"max_fan_out":8,"max_depth":2,"max_objects":8,"max_bytes":8192,"max_rows":0},"observation_binding":{"stream_id":"descendant-stream","source_pattern":"sessions/*/children/**","relative_selector":"**"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const COMPOSED_ROOT_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const COMPOSED_RELATED_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"team-config-from-evidence","primitive":"ReferencedObjectFromField","access_root":"root","locator":"teams/{team-name}/config.json","identity_inputs":["team-name"],"bounds":{"max_fan_out":4,"max_depth":1,"max_objects":4,"max_bytes":4096,"max_rows":0},"observation_binding":{"stream_id":"team-config-stream","source_pattern":"teams/*/config.json"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const COMPOSED_TWO_APPEND_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"sibling-object","primitive":"KnownObject","access_root":"root","locator":"sibling-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"sibling-stream","source_pattern":"siblings/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    struct PromotedTestSupportDocuments<'document> {
        support_release_id: &'document str,
        adapter_id: &'document str,
        ads_id: &'document str,
        artifact_family: &'document str,
        artifact_version: &'document str,
        required_marker: &'document str,
        adapter_package_version: &'document str,
        decoder_contract_version: u32,
        capabilities: &'document [(&'document str, &'document str)],
        source_document: &'document [u8],
        scope_document: &'document [u8],
    }

    fn promoted_test_catalog(
        config: PromotedTestSupportDocuments<'_>,
    ) -> (
        Arc<SupportCatalog>,
        AdapterSupportBinding,
        crate::adapter::ScopeProgramManifest,
    ) {
        let ads_document = serde_json::to_vec(&serde_json::json!({
            "adapter_id": config.adapter_id,
            "ads_id": config.ads_id,
        }))
        .unwrap();
        let evidence_document = ads_document.clone();
        let conformance_document = serde_json::to_vec(&serde_json::json!({
            "adapter_id": config.adapter_id,
            "support_release_id": config.support_release_id,
        }))
        .unwrap();
        let documents = [
            ("ads", "support/ads.json", ads_document),
            (
                "source_declaration",
                "support/source.json",
                config.source_document.to_vec(),
            ),
            (
                "scope_program",
                "support/scope.json",
                config.scope_document.to_vec(),
            ),
            ("evidence", "support/evidence.json", evidence_document),
            (
                "conformance",
                "support/conformance.json",
                conformance_document,
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
        let capabilities = config
            .capabilities
            .iter()
            .map(|(capability_id, topology)| {
                serde_json::json!({
                    "capability_id": capability_id,
                    "topology": topology,
                    "level": "supported",
                    "notes": null
                })
            })
            .collect::<Vec<_>>();
        let release_json = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "support_release_id": config.support_release_id,
            "adapter_id": config.adapter_id,
            "status": "promoted",
            "artifact_compatibility": {
                "family": config.artifact_family,
                "platforms": ["test"],
                "exact_versions": [config.artifact_version],
                "ranges": [],
                "required_markers": [config.required_marker],
                "forward_catalog_only": false
            },
            "references": references,
            "versions": {
                "adapter_package": config.adapter_package_version,
                "decoder_contract": config.decoder_contract_version
            },
            "capabilities": capabilities
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

    fn promoted_fixture_catalog_with_scope(
        scope_document: &[u8],
    ) -> (
        Arc<SupportCatalog>,
        AdapterSupportBinding,
        crate::adapter::ScopeProgramManifest,
    ) {
        let source_document = if scope_document == UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT
            || scope_document == JOIN_DERIVED_DYNAMIC_SCOPE_DOCUMENT
        {
            br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"artifact-blobs","root_id":"artifact","relative_patterns":["artifacts/*"],"primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"},{"stream_id":"descendant-stream","root_id":"root","relative_patterns":["sessions/*/children/**"],"decoder_id":"fixture-descendant","authority":"canonical","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#.as_slice()
        } else if scope_document == COMPOSED_RELATED_SCOPE_DOCUMENT {
            br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"team-config-stream","root_id":"root","relative_patterns":["teams/*/config.json"],"decoder_id":"fixture-related","authority":"canonical","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":4096},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#.as_slice()
        } else if scope_document == COMPOSED_TWO_APPEND_SCOPE_DOCUMENT {
            br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"sibling-stream","root_id":"root","relative_patterns":["siblings/*.jsonl"],"decoder_id":"fixture-sibling","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"}]}"#.as_slice()
        } else if scope_document == COMPOSED_ROOT_SCOPE_DOCUMENT {
            br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"}]}"#.as_slice()
        } else {
            br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"artifact-blobs","root_id":"artifact","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#.as_slice()
        };
        promoted_test_catalog(PromotedTestSupportDocuments {
            support_release_id: "fixture-release",
            adapter_id: "fixture",
            ads_id: "fixture-ads",
            artifact_family: "fixture",
            artifact_version: "1.0.0",
            required_marker: "fixture.marker",
            adapter_package_version: "1.0.0",
            decoder_contract_version: 1,
            capabilities: &[
                ("fixture-catalog", "catalog"),
                ("fixture-history", "durable"),
                ("fixture-observation", "scoped"),
            ],
            source_document,
            scope_document,
        })
    }

    fn promoted_fixture_catalog() -> (
        Arc<SupportCatalog>,
        AdapterSupportBinding,
        crate::adapter::ScopeProgramManifest,
    ) {
        promoted_fixture_catalog_with_scope(SINGLE_OBJECT_SCOPE_DOCUMENT)
    }

    impl AgentAdapter for EmptyAdapter {
        /// The surviving registry tests resolve adapters and authorize scope
        /// programs; none of them discovers instances from configured roots.
        fn discover(
            &self,
            _context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            Ok(Vec::new())
        }
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(self.streams.clone())
        }

        fn bootstrap_object(
            &self,
            _instance: &SourceInstance,
            _object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            Ok(AdapterObjectContext::empty())
        }

        /// The surviving registry tests never decode; the fixture adapter only
        /// has to satisfy the trait.
        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            Ok(DecodeDisposition::IgnoredKnown)
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
    fn native_support_probe_is_host_registered_exact_and_panic_contained() {
        let unregistered = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("one"))
            .register_native_support_probe("missing", |_| unreachable!())
            .build();
        assert!(unregistered.is_err());

        let duplicate = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("one"))
            .register_native_support_probe("one", |_| unreachable!())
            .register_native_support_probe("one", |_| unreachable!())
            .build();
        assert!(duplicate.is_err());

        let registry = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("one"))
            .register(EmptyAdapter::new("two"))
            .register_native_support_probe("one", |_| {
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build()
            .unwrap();
        let probe = registry
            .probe_native_support(&AdapterId::new("one").unwrap(), &[])
            .unwrap()
            .unwrap();
        assert_eq!(probe.family, "fixture");
        assert!(registry
            .probe_native_support(&AdapterId::new("two").unwrap(), &[])
            .unwrap()
            .is_none());

        let panicking = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("one"))
            .register_native_support_probe("one", |_| panic!("private probe detail"))
            .build()
            .unwrap();
        let error = panicking
            .probe_native_support(&AdapterId::new("one").unwrap(), &[])
            .unwrap_err();
        assert_eq!(error.code, "native_support_probe_panic");
        assert!(!error.to_string().contains("private probe detail"));
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

        let probe = NativeArtifactProbe {
            family: "fixture".to_string(),
            platform: "test".to_string(),
            version: Some("1.0.0".to_string()),
            markers: vec!["fixture.marker".to_string()],
            contradictory_markers: false,
        };
        let durable_authorization = registry
            .authorize_durable_if_supported(&AdapterId::new("fixture").unwrap(), &probe)
            .unwrap()
            .unwrap();
        assert_eq!(
            durable_authorization.operation().operation(),
            SupportOperation::DurableHistoryRuntime
        );

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
                &probe,
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
        let (scoped_decision, scoped_authorization) = registry
            .authorize_scoped_if_supported(
                &AdapterId::new("fixture").unwrap(),
                &NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                },
                &scoped_request,
                &scoped_offer,
            )
            .unwrap()
            .unwrap();
        assert!(scoped_decision.permissions().scoped_observation);
        assert_eq!(
            scoped_authorization.operation().operation(),
            SupportOperation::ScopedTypedObservation
        );
        let program = scoped_authorization
            .select_scope_program("observe-session")
            .unwrap();
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program).unwrap();
        assert_eq!(plan.adapter_id(), "fixture");
        assert_eq!(plan.support_release_id(), "fixture-release");

        let unsupported_probe = NativeArtifactProbe {
            family: "fixture".to_string(),
            platform: "test".to_string(),
            version: Some("9.9.9".to_string()),
            markers: vec!["fixture.marker".to_string()],
            contradictory_markers: false,
        };
        assert!(registry
            .authorize_scoped_if_supported(
                &AdapterId::new("fixture").unwrap(),
                &unsupported_probe,
                &scoped_request,
                &scoped_offer,
            )
            .unwrap()
            .is_none());

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
            "sha256:4fa5890dac3fbf65b3ebdb38d5e6cc82bef9e02e23cb6d3bb9aadf2792cf8105"
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
    fn candidate_support_cannot_authorize_a_promoted_dynamic_program() {
        let source_document = br#"{"adapter_id":"fixture","ads_id":"fixture-ads","streams":[{"stream_id":"root-stream","root_id":"root","relative_patterns":["sessions/*.jsonl"],"decoder_id":"fixture-root","authority":"canonical","primitive":"AppendDelimited","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_record_bytes":4194304,"max_batch_bytes":8388608,"max_records_per_batch":1024},"lifecycle":["append","partial_write","truncate","identity_change","delete","recreate"],"safe_decoder_state_boundary":"object_generation_cursor"},{"stream_id":"artifact-blobs","root_id":"artifact","relative_patterns":["artifacts/*"],"primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"},{"stream_id":"descendant-stream","root_id":"root","relative_patterns":["sessions/*/children/**"],"decoder_id":"fixture-descendant","authority":"canonical","primitive":"ReplaceDocument","topologies":["scoped"],"implementation_state":"existing","bounds":{"max_object_bytes":1024},"lifecycle":["replace","delete","recreate"],"safe_decoder_state_boundary":"object_generation_revision"}]}"#;
        let documents = [
            (
                "ads",
                "support/ads.json",
                br#"{"adapter_id":"fixture","ads_id":"fixture-ads"}"#.as_slice(),
            ),
            (
                "source_declaration",
                "support/source.json",
                source_document.as_slice(),
            ),
            (
                "scope_program",
                "support/scope.json",
                UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT,
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
            "status": "candidate",
            "artifact_compatibility": {
                "family": "fixture",
                "platforms": ["test"],
                "exact_versions": ["1.0.0"],
                "ranges": [],
                "required_markers": ["fixture.marker"],
                "forward_catalog_only": false
            },
            "references": references,
            "versions": {"adapter_package": "1.0.0", "decoder_contract": 1},
            "capabilities": [
                {
                    "capability_id": "fixture-observation",
                    "topology": "scoped",
                    "level": "supported",
                    "notes": null
                }
            ]
        }))
        .unwrap();
        let bundle_documents = documents
            .iter()
            .map(|(_, path, bytes)| SupportBundleDocument::new(path, bytes))
            .collect::<Vec<_>>();
        let error = verify_support_release_bundle(&release_json, &bundle_documents).unwrap_err();
        assert!(error
            .to_string()
            .contains("scope-program status is incompatible with the support-release status"));
        assert!(!error.to_string().contains("descendant-objects"));
        assert!(!error.to_string().contains("sessions/"));
    }
}
