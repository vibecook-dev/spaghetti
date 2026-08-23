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
pub(crate) mod tests;
