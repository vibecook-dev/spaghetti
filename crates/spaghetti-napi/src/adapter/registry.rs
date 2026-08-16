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

    use crate::adapter::{
        verify_support_release_bundle, AdapterManifest, AdapterObjectContext,
        AdapterSupportBinding, DecodeContext, DecodeDisposition, DiscoveryContext, FactBatch,
        Sha256Digest, SourceInstance, SourceInstanceSpec, SourceObjectDescriptor, StreamSpec,
        SupportBundleDocument,
    };
    use crate::source::SourceRecord;

    use super::*;

    struct EmptyAdapter {
        manifest: AdapterManifest,
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
    }
}
