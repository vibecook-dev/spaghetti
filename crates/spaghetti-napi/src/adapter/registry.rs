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

    /// Select a promoted or explicitly forward-catalog-only contract without
    /// granting durable or scoped source authority. Complete-only catalog
    /// producers separately reject forward-recognized authority before I/O.
    pub(crate) fn authorize_catalog_if_supported(
        &self,
        adapter_id: &AdapterId,
        probe: &NativeArtifactProbe,
    ) -> Result<Option<TypedAccessAuthorization>, AdapterError> {
        self.authorize_supported_operation(
            adapter_id,
            probe,
            SupportOperation::CatalogDiscovery,
            &["catalog.project", "catalog.session"],
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
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use crate::adapter::{
        verify_support_release_bundle, AdapterErrorClass, AdapterManifest, AdapterObjectContext,
        AdapterSupportBinding, AuthorizedObservationSourceDriver, CanonicalEntityKey,
        CanonicalFactId, CanonicalSourceInstanceKey, CompatibilityClass, ConsistencyPolicy,
        CoverageAbsenceKind, CoverageDomain, CoverageObjectKey, CoveragePositionKind,
        CoverageSetCompleteness, CoverageStatus, CoverageStreamKey, DecodeContext,
        DecodeDisposition, DecoderId, DeletionPolicy, DiscoveryContext, DriverSpec, EntityScope,
        ExternalEntityRef, Fact, FactBatch, FactRevisionId, FactSemanticContext,
        NativeArtifactProbe, ObjectSelector, RawRetentionPolicy, ScopeJoinEvidence,
        ScopeJoinIdentityInput, ScopeJoinParameterSet, ScopeJoinUpdate, ScopeRelationPrimitive,
        SemanticRevisionRef, Sha256Digest, SourceAccess, SourceInstance, SourceInstanceKey,
        SourceInstanceSpec, SourceObjectDescriptor, SourceRoot, StreamAuthority, StreamId,
        StreamSpec, SupportBundleDocument,
    };
    use crate::observation_contract::unknown_wire::{
        ObservationUnknownWireCapability, ObservationUnknownWireCompatibilityAxis,
        ObservationUnknownWireContractError, ObservationUnknownWireContractOffer,
        ObservationUnknownWireContractRequest,
    };
    use crate::observation_contract::{
        negotiate_observation_contract, ObservationCapabilities, ObservationCompatibilityAxis,
        ObservationContractOffer, ObservationContractRequest, ObservationNegotiationError,
    };
    use crate::scoped_observation::configured_attachment::{
        prepare_configured_scoped_observation_attachment,
        ConfiguredScopedObservationRuntimeOptions, ConfiguredScopedObservationSupervisorRunResult,
        PreparedScopedRelatedSourceObservation, ScopedConfiguredAttachmentRequest,
        ScopedConfiguredRootIdentity,
    };
    use crate::scoped_observation::{
        bind_observation_runtime_source_for_test,
        prepare_observation_directory_membership_for_test, prepare_scoped_observation_support,
        scan_observation_directory_membership_for_test,
        scan_observation_directory_membership_with_foreign_attachment_for_test,
        ScopedAccessRootGrant, ScopedActorAttribution, ScopedActorFallbackReason,
        ScopedAdmissionError, ScopedAppendDecodeOutcome, ScopedAppendDecoderConfig,
        ScopedAppendDeliveryPhase, ScopedAppendObservation, ScopedAppendPresenceChange,
        ScopedAppendReconcileRequest, ScopedArtifactAccessPolicy, ScopedArtifactContentPolicy,
        ScopedBootstrapBarrierError, ScopedContinuityError, ScopedCoverageAssemblyError,
        ScopedDecodeFailureClass, ScopedDecodedAppendItem, ScopedDeliveryError,
        ScopedEnvelopeEvidenceAuthority, ScopedKnownAppendObject, ScopedKnownObjectGrant,
        ScopedKnownObjectReadRequest, ScopedObjectRead, ScopedObservationAccessError,
        ScopedObservationAccessHost, ScopedObservationAccessPass, ScopedObservationAccessRequest,
        ScopedObservationAdmissionLane, ScopedObservationAppendPassBinding,
        ScopedObservationAppendPassRequest, ScopedObservationAsyncHandle,
        ScopedObservationAsyncOwnerFirstExit, ScopedObservationAsyncOwnerPair,
        ScopedObservationAsyncOwnerRunResult, ScopedObservationAsyncRuntime,
        ScopedObservationAutomaticResyncError, ScopedObservationCloseError,
        ScopedObservationConsumerOfferError, ScopedObservationContextualPollResolution,
        ScopedObservationContinuity, ScopedObservationDeliveryLane,
        ScopedObservationDeliveryLimits, ScopedObservationDirectoryMemberLifecycle,
        ScopedObservationDirectoryMemberObserveFailureKind, ScopedObservationDirectoryMemberRead,
        ScopedObservationDirectoryScan, ScopedObservationEvent,
        ScopedObservationNativeWatchBackend, ScopedObservationNativeWatchCallback,
        ScopedObservationNativeWatcherError, ScopedObservationNativeWatcherRecoveryPolicy,
        ScopedObservationNativeWatcherRunExit, ScopedObservationOpenDrainError,
        ScopedObservationOwnedIdentityInput, ScopedObservationPassExecutionError,
        ScopedObservationPollError, ScopedObservationPollLease, ScopedObservationPollResolution,
        ScopedObservationProjectionLimits, ScopedObservationProjectionSink,
        ScopedObservationQueueLimits, ScopedObservationReadyResolution,
        ScopedObservationRelatedObjectState, ScopedObservationResyncResolution,
        ScopedObservationScopeJoinSnapshot, ScopedObservationSourceOwnerBindingError,
        ScopedObservationSourceOwnerRetryPolicy, ScopedObservationSourceOwnerRunError,
        ScopedObservationSourceOwnerRunExit, ScopedObservationStartupError,
        ScopedObservationStartupReconcileAction, ScopedObservationTrustedAccessRequest,
        ScopedObservationUnknownWireNegotiation, ScopedObservationWatcherHintAction,
        ScopedObservationWatcherPhase, ScopedObserverFailureReason, ScopedProjectionDeliveryError,
        ScopedQueuedObservationFrame, ScopedRelationMembershipObservation, ScopedReplacementMode,
        ScopedReplacementRepresentation, ScopedReplacementStageError, ScopedResyncReason,
        ScopedRootIdentityRequest, ScopedScopeRelationState, ScopedSourceFailureClass,
        ScopedSourceObjectFailureCode, ScopedSourceObjectRetryState,
    };
    use crate::source::{
        confined_relative_path_key, platform_path_key, AccessObjectToken, AccessOperation,
        AccessOutcome, AccessPhase, AppendDelimitedConfig, AppendDelimitedFile, AppendItem,
        AppendRead, AppendTransition, AuthorizedScopeAccessPlan, DirectoryEntryKind,
        DirectorySelection, DirtyHint, DirtyReason, DirtyScope, HintEnqueue, IngestPriority,
        RecordHash, RecordOrigin, ReplaceDocumentConfig, Revision, ScopeAccessReport,
        ScopeAccessRequest, ScopeIdentityInput, SharedSourcePassPool, SourceCursor,
        SourceMediaType, SourceRecord, SourceRecordState,
    };

    use super::*;

    struct ImmediateScopedWatchBackend {
        callback: Box<dyn FnMut(notify::Result<notify::Event>) + Send + 'static>,
        target: PathBuf,
        unrelated: PathBuf,
        registrations: Arc<std::sync::Mutex<Vec<(PathBuf, notify::RecursiveMode)>>>,
        fail_registration: bool,
        emit_rescan: bool,
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
            if self.emit_rescan {
                (self.callback)(Ok(notify::Event::new(notify::EventKind::Other)
                    .set_flag(notify::event::Flag::Rescan)));
            }
            Ok(())
        }
    }

    struct ControlledScopedWatchBackend {
        callback: Option<ScopedObservationNativeWatchCallback>,
        callback_slot: Arc<std::sync::Mutex<Option<ScopedObservationNativeWatchCallback>>>,
        registrations: Arc<std::sync::Mutex<Vec<(PathBuf, notify::RecursiveMode)>>>,
        drops: Arc<AtomicUsize>,
    }

    impl ScopedObservationNativeWatchBackend for ControlledScopedWatchBackend {
        fn watch(&mut self, path: &std::path::Path, mode: notify::RecursiveMode) -> Result<(), ()> {
            self.registrations
                .lock()
                .unwrap()
                .push((path.to_path_buf(), mode));
            if let Some(callback) = self.callback.take() {
                *self.callback_slot.lock().unwrap() = Some(callback);
            }
            Ok(())
        }
    }

    impl Drop for ControlledScopedWatchBackend {
        fn drop(&mut self) {
            self.callback_slot.lock().unwrap().take();
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct EmptyAdapter {
        manifest: AdapterManifest,
        streams: Vec<StreamSpec>,
        panic_streams: bool,
        discover_configured_roots: bool,
        discover_calls: Option<Arc<AtomicUsize>>,
        decode_statefully: bool,
        request_dependency_access: bool,
        dependency_mutation: Option<(PathBuf, Vec<u8>)>,
        dependency_free_bootstrap_failure_suffix: Option<PathBuf>,
        dependency_free_bootstrap_panic_suffix: Option<PathBuf>,
        dependency_free_context_revision: Option<Arc<AtomicUsize>>,
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
                panic_streams: false,
                discover_configured_roots: false,
                discover_calls: None,
                decode_statefully: false,
                request_dependency_access: false,
                dependency_mutation: None,
                dependency_free_bootstrap_failure_suffix: None,
                dependency_free_bootstrap_panic_suffix: None,
                dependency_free_context_revision: None,
            }
        }

        fn with_stateful_decode(mut self) -> Self {
            self.decode_statefully = true;
            self
        }

        fn with_streams(mut self, streams: Vec<StreamSpec>) -> Self {
            self.streams = streams;
            self
        }

        fn with_stream_panic(mut self) -> Self {
            self.panic_streams = true;
            self
        }

        fn with_configured_root_discovery(mut self, calls: Arc<AtomicUsize>) -> Self {
            self.discover_configured_roots = true;
            self.discover_calls = Some(calls);
            self
        }

        fn with_dependency_access(mut self) -> Self {
            self.request_dependency_access = true;
            self
        }

        fn with_dependency_mutation(mut self, path: PathBuf, payload: Vec<u8>) -> Self {
            self.dependency_mutation = Some((path, payload));
            self
        }

        fn with_dependency_free_bootstrap_failure(mut self, suffix: PathBuf) -> Self {
            self.dependency_free_bootstrap_failure_suffix = Some(suffix);
            self
        }

        fn with_dependency_free_bootstrap_panic(mut self, suffix: PathBuf) -> Self {
            self.dependency_free_bootstrap_panic_suffix = Some(suffix);
            self
        }

        fn with_dependency_free_context_revision(mut self, revision: Arc<AtomicUsize>) -> Self {
            self.dependency_free_context_revision = Some(revision);
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

    const SINGLE_OBJECT_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const TWO_OBJECT_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"sibling-object","primitive":"KnownObject","access_root":"root","locator":"sibling-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const DEPENDENCY_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"decoder-sidecar","primitive":"KnownObject","access_root":"root","locator":"decoder-sidecar","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const CLAUDE_COMPOSED_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-transcript","relations":[{"relation_id":"root-transcript","primitive":"KnownObject","access_root":"root","locator":"root-transcript","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"current-child","primitive":"KnownObject","access_root":"root","locator":"current-child","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"future-child","primitive":"KnownObject","access_root":"root","locator":"future-child","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"team-inbox-sidecar","primitive":"KnownObject","access_root":"root","locator":"team-inbox-sidecar","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"descendant-objects","primitive":"ChildDirectoryByNativeId","access_root":"root","locator":"sessions/{native-session-id}/children","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":8,"max_depth":2,"max_objects":8,"max_bytes":8192,"max_rows":0},"observation_binding":{"stream_id":"descendant-stream","source_pattern":"sessions/*/children/**","relative_selector":"**"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const COMPOSED_ROOT_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const COMPOSED_RELATED_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"team-config-from-evidence","primitive":"ReferencedObjectFromField","access_root":"root","locator":"teams/{team-name}/config.json","identity_inputs":["team-name"],"bounds":{"max_fan_out":4,"max_depth":1,"max_objects":4,"max_bytes":4096,"max_rows":0},"observation_binding":{"stream_id":"team-config-stream","source_pattern":"teams/*/config.json"},"unavailable_behavior":"skip_optional","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    const COMPOSED_TWO_APPEND_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"root-stream","source_pattern":"sessions/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"sibling-object","primitive":"KnownObject","access_root":"root","locator":"sibling-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":8388608,"max_rows":0},"observation_binding":{"stream_id":"sibling-stream","source_pattern":"siblings/*.jsonl"},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    fn promoted_fixture_catalog_with_scope(
        scope_document: &[u8],
    ) -> (
        Arc<SupportCatalog>,
        AdapterSupportBinding,
        crate::adapter::ScopeProgramManifest,
    ) {
        let source_document = if scope_document == UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT {
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
        let documents = [
            (
                "ads",
                "support/ads.json",
                br#"{"adapter_id":"fixture","ads_id":"fixture-ads"}"#.as_slice(),
            ),
            ("source_declaration", "support/source.json", source_document),
            ("scope_program", "support/scope.json", scope_document),
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
            "versions": {"adapter_package": "1.0.0", "decoder_contract": 1},
            "capabilities": [
                {
                    "capability_id": "fixture-catalog",
                    "topology": "catalog",
                    "level": "supported",
                    "notes": null
                },
                {
                    "capability_id": "fixture-history",
                    "topology": "durable",
                    "level": "supported",
                    "notes": null
                },
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
        let release = verify_support_release_bundle(&release_json, &bundle_documents).unwrap();
        let binding = release.adapter_binding().clone();
        let scope_programs = release.scope_programs().clone();
        (
            Arc::new(SupportCatalog::new([release]).unwrap()),
            binding,
            scope_programs,
        )
    }

    fn promoted_fixture_catalog() -> (
        Arc<SupportCatalog>,
        AdapterSupportBinding,
        crate::adapter::ScopeProgramManifest,
    ) {
        promoted_fixture_catalog_with_scope(SINGLE_OBJECT_SCOPE_DOCUMENT)
    }

    fn supported_fixture_registry() -> AdapterRegistry {
        let (catalog, binding, scope_programs) = promoted_fixture_catalog();
        AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("fixture").with_support(binding, scope_programs))
            .build_supported(catalog)
            .unwrap()
    }

    pub(crate) fn supported_fixture_registry_with_scope(scope_document: &[u8]) -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(scope_document);
        let adapter = EmptyAdapter::new("fixture").with_support(binding, scope_programs);
        let adapter = if scope_document == UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT {
            adapter.with_streams(vec![
                fixture_root_runtime_stream(),
                fixture_descendant_runtime_stream(),
            ])
        } else {
            adapter
        };
        AdapterRegistryBuilder::new()
            .register(adapter)
            .build_supported(catalog)
            .unwrap()
    }

    fn fixture_descendant_runtime_stream() -> StreamSpec {
        StreamSpec {
            id: StreamId::new("descendant-stream").unwrap(),
            driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: 1_024,
            }),
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["sessions/*/children/**".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new("fixture-descendant").unwrap(),
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Session,
            priority: IngestPriority::Interactive,
            consistency: ConsistencyPolicy::SnapshotReplace,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: Vec::new(),
        }
    }

    fn fixture_related_runtime_stream() -> StreamSpec {
        StreamSpec {
            id: StreamId::new("team-config-stream").unwrap(),
            driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: 4_096,
            }),
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["teams/*/config.json".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new("fixture-related").unwrap(),
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Session,
            priority: IngestPriority::Interactive,
            consistency: ConsistencyPolicy::SnapshotReplace,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: Vec::new(),
        }
    }

    fn fixture_root_runtime_stream() -> StreamSpec {
        StreamSpec {
            id: StreamId::new("root-stream").unwrap(),
            driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["sessions/*.jsonl".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new("fixture-root").unwrap(),
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Session,
            priority: IngestPriority::Interactive,
            consistency: ConsistencyPolicy::IncrementalCursor,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: Vec::new(),
        }
    }

    fn fixture_sibling_runtime_stream() -> StreamSpec {
        StreamSpec {
            id: StreamId::new("sibling-stream").unwrap(),
            driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["siblings/*.jsonl".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new("fixture-sibling").unwrap(),
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Session,
            priority: IngestPriority::Interactive,
            consistency: ConsistencyPolicy::IncrementalCursor,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: Vec::new(),
        }
    }

    fn configured_attachment_registry(
        probe_calls: Arc<AtomicUsize>,
        discover_calls: Arc<AtomicUsize>,
        probe_version: &'static str,
    ) -> AdapterRegistry {
        configured_attachment_registry_with_stream_behavior(
            probe_calls,
            discover_calls,
            probe_version,
            false,
        )
    }

    fn configured_attachment_registry_with_stream_behavior(
        probe_calls: Arc<AtomicUsize>,
        discover_calls: Arc<AtomicUsize>,
        probe_version: &'static str,
        panic_streams: bool,
    ) -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(COMPOSED_ROOT_SCOPE_DOCUMENT);
        let mut adapter = EmptyAdapter::new("fixture")
            .with_support(binding, scope_programs)
            .with_streams(vec![fixture_root_runtime_stream()])
            .with_configured_root_discovery(discover_calls);
        if panic_streams {
            adapter = adapter.with_stream_panic();
        }
        AdapterRegistryBuilder::new()
            .register(adapter)
            .register_native_support_probe("fixture", move |_| {
                probe_calls.fetch_add(1, Ordering::AcqRel);
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some(probe_version.to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap()
    }

    fn configured_attachment_registry_with_decoder_bootstrap_failure(
        probe_calls: Arc<AtomicUsize>,
        discover_calls: Arc<AtomicUsize>,
    ) -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(COMPOSED_ROOT_SCOPE_DOCUMENT);
        AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![fixture_root_runtime_stream()])
                    .with_configured_root_discovery(discover_calls)
                    .with_dependency_free_bootstrap_failure(PathBuf::from("session.jsonl")),
            )
            .register_native_support_probe("fixture", move |_| {
                probe_calls.fetch_add(1, Ordering::AcqRel);
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap()
    }

    fn configured_two_append_registry(
        probe_calls: Arc<AtomicUsize>,
        discover_calls: Arc<AtomicUsize>,
    ) -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(COMPOSED_TWO_APPEND_SCOPE_DOCUMENT);
        AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![
                        fixture_root_runtime_stream(),
                        fixture_sibling_runtime_stream(),
                    ])
                    .with_configured_root_discovery(discover_calls)
                    .with_stateful_decode(),
            )
            .register_native_support_probe("fixture", move |_| {
                probe_calls.fetch_add(1, Ordering::AcqRel);
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap()
    }

    fn configured_dynamic_directory_registry(
        probe_calls: Arc<AtomicUsize>,
        discover_calls: Arc<AtomicUsize>,
    ) -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![
                        fixture_root_runtime_stream(),
                        fixture_descendant_runtime_stream(),
                    ])
                    .with_configured_root_discovery(discover_calls),
            )
            .register_native_support_probe("fixture", move |_| {
                probe_calls.fetch_add(1, Ordering::AcqRel);
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap()
    }

    fn configured_related_object_registry(
        probe_calls: Arc<AtomicUsize>,
        discover_calls: Arc<AtomicUsize>,
    ) -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(COMPOSED_RELATED_SCOPE_DOCUMENT);
        AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![
                        fixture_root_runtime_stream(),
                        fixture_related_runtime_stream(),
                    ])
                    .with_stateful_decode()
                    .with_configured_root_discovery(discover_calls),
            )
            .register_native_support_probe("fixture", move |_| {
                probe_calls.fetch_add(1, Ordering::AcqRel);
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap()
    }

    fn configured_attachment_request(
        configured_roots: Vec<PathBuf>,
        relative_path: PathBuf,
    ) -> ScopedConfiguredAttachmentRequest {
        let identity = ScopedConfiguredRootIdentity::new(
            b"fixture-session".as_slice(),
            BTreeMap::from([(
                "native-session-id".to_string(),
                Arc::<[u8]>::from(b"fixture-session".as_slice()),
            )]),
        )
        .unwrap()
        .with_root_run_identity_key(Arc::from(b"fixture-root-run".as_slice()));
        configured_attachment_request_with_identity(configured_roots, relative_path, identity)
    }

    fn configured_attachment_request_with_identity(
        configured_roots: Vec<PathBuf>,
        relative_path: PathBuf,
        identity: ScopedConfiguredRootIdentity,
    ) -> ScopedConfiguredAttachmentRequest {
        let template = scoped_access_request(configured_roots[0].clone());
        ScopedConfiguredAttachmentRequest::new(
            "fixture",
            configured_roots,
            "observe-session",
            BTreeMap::from([("root-object".to_string(), relative_path)]),
            identity,
            template.observation_contract_request,
            template.observation_contract_offer,
        )
        .unwrap()
        .with_unknown_wire_contract(template.unknown_wire_contract.unwrap())
    }

    fn configured_two_append_request(root: PathBuf) -> ScopedConfiguredAttachmentRequest {
        let identity = ScopedConfiguredRootIdentity::new(
            b"fixture-session".as_slice(),
            BTreeMap::from([(
                "native-session-id".to_string(),
                Arc::<[u8]>::from(b"fixture-session".as_slice()),
            )]),
        )
        .unwrap()
        .with_root_run_identity_key(Arc::from(b"fixture-root-run".as_slice()));
        let template = scoped_access_request(root.clone());
        ScopedConfiguredAttachmentRequest::new(
            "fixture",
            vec![root],
            "observe-session",
            BTreeMap::from([
                (
                    "root-object".to_string(),
                    PathBuf::from("sessions/session.jsonl"),
                ),
                (
                    "sibling-object".to_string(),
                    PathBuf::from("siblings/sibling.jsonl"),
                ),
            ]),
            identity,
            template.observation_contract_request,
            template.observation_contract_offer,
        )
        .unwrap()
        .with_unknown_wire_contract(template.unknown_wire_contract.unwrap())
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

    fn stateful_two_object_fixture_registry() -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(TWO_OBJECT_SCOPE_DOCUMENT);
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

    fn declared_dependency_fixture_registry(
        mutation: Option<(PathBuf, Vec<u8>)>,
    ) -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(DEPENDENCY_SCOPE_DOCUMENT);
        let mut adapter = EmptyAdapter::new("fixture")
            .with_support(binding, scope_programs)
            .with_stateful_decode()
            .with_dependency_access();
        if let Some((path, payload)) = mutation {
            adapter = adapter.with_dependency_mutation(path, payload);
        }
        AdapterRegistryBuilder::new()
            .register(adapter)
            .build_supported(catalog)
            .unwrap()
    }

    pub(crate) fn scoped_access_request(root: PathBuf) -> ScopedObservationAccessRequest {
        ScopedObservationAccessRequest {
            adapter_id: "fixture".to_string(),
            artifact_probe: NativeArtifactProbe {
                family: "fixture".to_string(),
                platform: "test".to_string(),
                version: Some("1.0.0".to_string()),
                markers: vec!["fixture.marker".to_string()],
                contradictory_markers: false,
            },
            source_instance: SourceInstance {
                id: 1,
                spec: SourceInstanceSpec {
                    identity_contract_version: 1,
                    stable_key: SourceInstanceKey::new(b"fixture-source-instance".to_vec())
                        .unwrap(),
                    display_name: "fixture".to_string(),
                    roots: vec![SourceRoot {
                        name: "root".to_string(),
                        path: root.clone(),
                    }],
                    discovery_reason: "fixture".to_string(),
                },
            },
            artifact_access_policy: ScopedArtifactAccessPolicy::bounded(
                8 * 1024 * 1024,
                ScopedArtifactContentPolicy::Inline,
            ),
            observation_contract_request: ObservationContractRequest::new(
                ContractVersionRequest {
                    selection_contract_version: 1,
                    model_major: 1,
                    external_entity_reference_version: 1,
                    semantic_revision_reference_version: 1,
                    coverage_contract_versions: vec![1],
                    fact_family_versions: BTreeMap::from([(
                        "runtime.usage-v2".to_string(),
                        vec![1],
                    )]),
                    query_pack_versions: None,
                    observation_contract_versions: Some(vec![1]),
                },
                vec![1],
                vec![1],
                vec![1],
            )
            .unwrap(),
            observation_contract_offer: ObservationContractOffer::new(
                ContractVersionOffer {
                    selection_contract_version: 1,
                    model_major: 1,
                    external_entity_reference_versions: vec![1],
                    semantic_revision_reference_versions: vec![1],
                    coverage_contract_versions: vec![1],
                    fact_family_versions: BTreeMap::from([(
                        "runtime.usage-v2".to_string(),
                        vec![1],
                    )]),
                    query_pack_versions: Vec::new(),
                    observation_contract_versions: vec![1],
                },
                vec![1],
                vec![1],
                vec![1],
            )
            .unwrap(),
            unknown_wire_contract: Some(ScopedObservationUnknownWireNegotiation::new(
                ObservationUnknownWireContractRequest::new(
                    ObservationUnknownWireCapability::preserving(8_192).unwrap(),
                )
                .unwrap(),
                ObservationUnknownWireContractOffer::new(
                    ObservationUnknownWireCapability::preserving(4_096).unwrap(),
                )
                .unwrap(),
            )),
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
                root: root.clone(),
                relative_path: "session.jsonl".into(),
            }],
            access_roots: vec![ScopedAccessRootGrant {
                access_root: "root".to_string(),
                root,
            }],
            artifact_relations: Vec::new(),
        }
    }

    fn multi_family_scoped_access_request(root: PathBuf) -> ScopedObservationAccessRequest {
        let mut request = scoped_access_request(root);
        let families = BTreeMap::from([
            ("runtime.actor-affiliation".to_owned(), vec![1]),
            ("runtime.actor-run".to_owned(), vec![1]),
            ("runtime.usage-v2".to_owned(), vec![1]),
        ]);
        request.observation_contract_request = ObservationContractRequest::new(
            ContractVersionRequest {
                selection_contract_version: 1,
                model_major: 1,
                external_entity_reference_version: 1,
                semantic_revision_reference_version: 1,
                coverage_contract_versions: vec![1],
                fact_family_versions: families.clone(),
                query_pack_versions: None,
                observation_contract_versions: Some(vec![1]),
            },
            vec![1],
            vec![1],
            vec![1],
        )
        .unwrap();
        request.observation_contract_offer = ObservationContractOffer::new(
            ContractVersionOffer {
                selection_contract_version: 1,
                model_major: 1,
                external_entity_reference_versions: vec![1],
                semantic_revision_reference_versions: vec![1],
                coverage_contract_versions: vec![1],
                fact_family_versions: families,
                query_pack_versions: Vec::new(),
                observation_contract_versions: vec![1],
            },
            vec![1],
            vec![1],
            vec![1],
        )
        .unwrap();
        request
    }

    fn two_object_scoped_access_request(root: PathBuf) -> ScopedObservationAccessRequest {
        let mut request = scoped_access_request(root.clone());
        request.known_objects.push(ScopedKnownObjectGrant {
            relation_id: "sibling-object".to_string(),
            scope_root: false,
            access_root: "root".to_string(),
            locator_id: "sibling-object".to_string(),
            root,
            relative_path: "sibling.jsonl".into(),
        });
        request
    }

    fn claude_composed_fixture_registry() -> AdapterRegistry {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(CLAUDE_COMPOSED_SCOPE_DOCUMENT);
        AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_stateful_decode(),
            )
            .build_supported(catalog)
            .unwrap()
    }

    fn claude_composed_scoped_access_request(root: PathBuf) -> ScopedObservationAccessRequest {
        let mut request = scoped_access_request(root.clone());
        request.known_objects = vec![
            ScopedKnownObjectGrant {
                relation_id: "root-transcript".to_string(),
                scope_root: true,
                access_root: "root".to_string(),
                locator_id: "root-transcript".to_string(),
                root: root.clone(),
                relative_path: "session.jsonl".into(),
            },
            ScopedKnownObjectGrant {
                relation_id: "current-child".to_string(),
                scope_root: false,
                access_root: "root".to_string(),
                locator_id: "current-child".to_string(),
                root: root.clone(),
                relative_path: "agent-current.jsonl".into(),
            },
            ScopedKnownObjectGrant {
                relation_id: "future-child".to_string(),
                scope_root: false,
                access_root: "root".to_string(),
                locator_id: "future-child".to_string(),
                root: root.clone(),
                relative_path: "agent-future.jsonl".into(),
            },
            ScopedKnownObjectGrant {
                relation_id: "team-inbox-sidecar".to_string(),
                scope_root: false,
                access_root: "root".to_string(),
                locator_id: "team-inbox-sidecar".to_string(),
                root,
                relative_path: "team-inbox.json".into(),
            },
        ];
        request
    }

    fn declared_dependency_scoped_access_request(root: PathBuf) -> ScopedObservationAccessRequest {
        let mut request = scoped_access_request(root.clone());
        request.known_objects.push(ScopedKnownObjectGrant {
            relation_id: "decoder-sidecar".to_string(),
            scope_root: false,
            access_root: "root".to_string(),
            locator_id: "decoder-sidecar".to_string(),
            root,
            relative_path: "sidecar.json".into(),
        });
        request
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

    fn scoped_append_object_for_native_object(native_object_key: &[u8]) -> ScopedKnownAppendObject {
        ScopedKnownAppendObject::new(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            ScopedAppendDecoderConfig {
                decoder: DecoderId::new("fixture-decoder").unwrap(),
                object_context: AdapterObjectContext::empty(),
                semantic_context: FactSemanticContext::new(
                    &AdapterId::new("fixture").unwrap(),
                    1,
                    b"fixture-source-instance",
                    b"fixture-transcript",
                    native_object_key,
                    1,
                )
                .unwrap(),
                coverage_domains: vec![CoverageDomain::FactFamily {
                    family: "runtime.usage-v2".to_string(),
                    version: 1,
                }],
                retention: RawRetentionPolicy::None,
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

    fn admission_lane_for_objects(max_coverage_objects: usize) -> ScopedObservationAdmissionLane {
        ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events: 16,
            max_retained_native_bytes: 0,
            max_control_items: 16,
            max_coverage_objects,
        })
        .unwrap()
    }

    struct AutomaticSingleObjectFixtureRoot {
        path: PathBuf,
        identity_value: Option<Vec<u8>>,
    }

    impl AutomaticSingleObjectFixtureRoot {
        fn existing(path: PathBuf) -> Self {
            Self {
                path,
                identity_value: None,
            }
        }

        fn distinct(path: PathBuf, identity_value: Vec<u8>) -> Self {
            Self {
                path,
                identity_value: Some(identity_value),
            }
        }
    }

    struct AutomaticSingleObjectOwnerPolicies {
        source: ScopedObservationSourceOwnerRetryPolicy,
        watcher: ScopedObservationNativeWatcherRecoveryPolicy,
        pass_pool: Option<SharedSourcePassPool>,
    }

    impl AutomaticSingleObjectOwnerPolicies {
        fn new(
            source: ScopedObservationSourceOwnerRetryPolicy,
            watcher: ScopedObservationNativeWatcherRecoveryPolicy,
        ) -> Self {
            Self {
                source,
                watcher,
                pass_pool: None,
            }
        }

        fn with_pass_pool(mut self, pass_pool: SharedSourcePassPool) -> Self {
            self.pass_pool = Some(pass_pool);
            self
        }
    }

    async fn automatic_single_object_pair_with_root_and_watcher_policy(
        registry: &AdapterRegistry,
        fixture_root: AutomaticSingleObjectFixtureRoot,
        identity_value: Vec<u8>,
        append_config: AppendDelimitedConfig,
        max_bytes: u64,
        policies: AutomaticSingleObjectOwnerPolicies,
    ) -> (
        ScopedObservationAsyncRuntime,
        ScopedObservationAsyncHandle,
        ScopedObservationAsyncOwnerPair,
        PathBuf,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<Option<ScopedObservationNativeWatchCallback>>>,
    ) {
        let AutomaticSingleObjectFixtureRoot {
            path: root,
            identity_value: root_identity_value,
        } = fixture_root;
        let AutomaticSingleObjectOwnerPolicies {
            source: source_policy,
            watcher: watcher_policy,
            pass_pool,
        } = policies;
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("session.jsonl");
        let mut request = scoped_access_request(root);
        if let Some(root_identity_value) = root_identity_value {
            request.root_identity = ScopedRootIdentityRequest::new(
                1,
                b"fixture-source-instance".as_slice(),
                root_identity_value,
                None,
                None,
                None,
            );
        }
        let host = ScopedObservationAccessHost::authorize(registry, request).unwrap();
        let limits = ScopedObservationDeliveryLimits {
            max_semantic_events: 4,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        };
        let mut runtime = match pass_pool {
            Some(pass_pool) => {
                ScopedObservationAsyncRuntime::open_with_shared_pass_pool(host, limits, pass_pool)
            }
            None => ScopedObservationAsyncRuntime::open(host, limits),
        }
        .unwrap();
        let handle = runtime.handle();
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let watcher = handle
            .install_native_watcher_with_factory(2, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(append_config).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let mut admission = admission_lane(4, 0, 2);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: identity_value.as_slice(),
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
        let observation = reconcile_scoped_append(
            handle.host(),
            scan.access_pass(),
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Initial,
        );
        assert!(!observation.object_present);
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
        assert!(matches!(
            watcher
                .coordinator()
                .next_reconcile(handle.host(), 2)
                .unwrap(),
            ScopedObservationStartupReconcileAction::CaughtUp
        ));
        object.complete_bootstrap().unwrap();
        let bootstrap = handle
            .with_attachment(|host, drain| {
                watcher.coordinator().offer_bootstrap_complete(
                    host,
                    std::slice::from_ref(&object),
                    &admission,
                    &projection,
                    drain,
                    50,
                )
            })
            .unwrap();
        let bootstrap_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &bootstrap_event.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { barrier }
                if Arc::ptr_eq(barrier, &bootstrap)
        ));
        runtime
            .acknowledge_applied(bootstrap_event.application_receipt())
            .unwrap();
        let active = handle
            .with_attachment(|host, drain| {
                host.bind_consumer_bootstrap_epoch_state(vec![object], admission, projection, drain)
            })
            .unwrap();
        let binding = ScopedObservationAppendPassBinding::new(
            "root-object",
            vec![
                ScopedObservationOwnedIdentityInput::new("native-session-id", identity_value)
                    .unwrap(),
            ],
            None,
            1,
            max_bytes,
            origin,
            false,
        )
        .unwrap();
        let source = handle
            .bind_epoch_source_owner(active, vec![binding], source_policy)
            .unwrap();
        let pair = handle
            .bind_live_owner_pair(watcher, source, watcher_policy)
            .unwrap();
        (runtime, handle, pair, target, drops, callback_slot)
    }

    async fn automatic_single_object_pair_with_watcher_policy(
        registry: &AdapterRegistry,
        root: PathBuf,
        identity_value: Vec<u8>,
        append_config: AppendDelimitedConfig,
        max_bytes: u64,
        source_policy: ScopedObservationSourceOwnerRetryPolicy,
        watcher_policy: ScopedObservationNativeWatcherRecoveryPolicy,
    ) -> (
        ScopedObservationAsyncRuntime,
        ScopedObservationAsyncHandle,
        ScopedObservationAsyncOwnerPair,
        PathBuf,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<Option<ScopedObservationNativeWatchCallback>>>,
    ) {
        automatic_single_object_pair_with_root_and_watcher_policy(
            registry,
            AutomaticSingleObjectFixtureRoot::existing(root),
            identity_value,
            append_config,
            max_bytes,
            AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy),
        )
        .await
    }

    async fn automatic_single_object_pair(
        registry: &AdapterRegistry,
        root: PathBuf,
        identity_value: Vec<u8>,
        append_config: AppendDelimitedConfig,
        max_bytes: u64,
        source_policy: ScopedObservationSourceOwnerRetryPolicy,
    ) -> (
        ScopedObservationAsyncRuntime,
        ScopedObservationAsyncHandle,
        ScopedObservationAsyncOwnerPair,
        PathBuf,
        Arc<AtomicUsize>,
    ) {
        let (runtime, handle, pair, target, drops, _) =
            automatic_single_object_pair_with_watcher_policy(
                registry,
                root,
                identity_value,
                append_config,
                max_bytes,
                source_policy,
                ScopedObservationNativeWatcherRecoveryPolicy::new(
                    std::time::Duration::from_secs(60),
                    std::time::Duration::from_millis(1),
                    std::time::Duration::from_millis(1),
                    1,
                )
                .unwrap(),
            )
            .await;
        (runtime, handle, pair, target, drops)
    }

    async fn acknowledge_automatic_resync_start(
        runtime: &mut ScopedObservationAsyncRuntime,
        invalid_scope_epoch: u64,
        new_scope_epoch: u64,
        expected_reason: ScopedResyncReason,
    ) {
        let required = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncRequired { control } = &required.envelope.event
        else {
            panic!("the bind-boundary race must remain a continuity control");
        };
        assert_eq!(required.envelope.scope_epoch, invalid_scope_epoch);
        assert_eq!(control.invalid_scope_epoch, invalid_scope_epoch);
        assert_eq!(control.reason, expected_reason);
        runtime
            .acknowledge_applied(required.application_receipt())
            .unwrap();

        let started = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncStarted { control } = &started.envelope.event
        else {
            panic!("the bind-boundary race must start one fresh epoch");
        };
        assert_eq!(control.old_scope_epoch, invalid_scope_epoch);
        assert_eq!(control.new_scope_epoch, new_scope_epoch);
        assert_eq!(started.envelope.scope_epoch, new_scope_epoch);
        runtime
            .acknowledge_applied(started.application_receipt())
            .unwrap();
    }

    async fn acknowledge_clean_automatic_resync_epoch(
        runtime: &mut ScopedObservationAsyncRuntime,
        invalid_scope_epoch: u64,
        new_scope_epoch: u64,
        expected_reason: ScopedResyncReason,
    ) {
        acknowledge_automatic_resync_start(
            runtime,
            invalid_scope_epoch,
            new_scope_epoch,
            expected_reason,
        )
        .await;
        let completed = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } = &completed.envelope.event
        else {
            panic!(
                "the recovered bind-boundary race must complete without observer failure: {:?}",
                completed.envelope.event
            );
        };
        assert_eq!(completed.envelope.scope_epoch, new_scope_epoch);
        assert_eq!(barrier.scope_epoch, new_scope_epoch);
        assert!(barrier.root_present);
        assert!(barrier.explicit_object_errors.is_empty());
        runtime
            .acknowledge_applied(completed.application_receipt())
            .unwrap();
    }

    fn reconcile_missing_relation_poll(
        host: &ScopedObservationAccessHost,
        lease: &ScopedObservationPollLease,
        relation_id: &str,
        object: &mut ScopedKnownAppendObject,
        admission: &mut ScopedObservationAdmissionLane,
        identity_inputs: &[ScopeIdentityInput<'_>],
        origin: &RecordOrigin,
    ) {
        let observation = reconcile_named_relation(
            host,
            lease.access_pass(),
            object,
            admission,
            named_relation_request(relation_id, identity_inputs, origin, AccessPhase::Initial),
        );
        assert!(!observation.object_present);
    }

    fn named_relation_request<'a>(
        relation_id: &'a str,
        identity_inputs: &'a [ScopeIdentityInput<'a>],
        origin: &'a RecordOrigin,
        access_phase: AccessPhase,
    ) -> ScopedAppendReconcileRequest<'a> {
        ScopedAppendReconcileRequest {
            relation_id,
            identity_inputs,
            access_phase,
            parent_token: None,
            depth: 1,
            max_bytes: 64,
            origin,
            force_contract_replay: false,
        }
    }

    fn reconcile_named_relation(
        host: &ScopedObservationAccessHost,
        pass: &ScopedObservationAccessPass,
        object: &mut ScopedKnownAppendObject,
        admission: &mut ScopedObservationAdmissionLane,
        request: ScopedAppendReconcileRequest<'_>,
    ) -> ScopedAppendObservation {
        let relation_id = request.relation_id;
        let observation = object.reconcile(pass, request).unwrap();
        let ScopedAppendDecodeOutcome::Ready(decoded) = decode_scoped(host, object, &observation)
        else {
            panic!("{relation_id} must produce a complete decoded observation");
        };
        if let Err(failure) = admission.admit(object, &observation, decoded) {
            panic!("{relation_id} admission failed: {}", failure.error);
        }
        observation
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
            context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            if !self.discover_configured_roots {
                return Ok(Vec::new());
            }
            if let Some(calls) = &self.discover_calls {
                calls.fetch_add(1, Ordering::AcqRel);
            }
            context
                .configured_roots
                .iter()
                .map(|root| {
                    Ok(SourceInstanceSpec {
                        identity_contract_version: 1,
                        stable_key: SourceInstanceKey::new(platform_path_key(root))?,
                        display_name: "configured fixture".to_string(),
                        roots: vec![SourceRoot {
                            name: "root".to_string(),
                            path: root.clone(),
                        }],
                        discovery_reason: "configured fixture root".to_string(),
                    })
                })
                .collect()
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            if self.panic_streams {
                panic!("fixture runtime stream panic");
            }
            Ok(self.streams.clone())
        }

        fn bootstrap_object(
            &self,
            _instance: &SourceInstance,
            _object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            Ok(AdapterObjectContext::empty())
        }

        fn bootstrap_object_without_source_access(
            &self,
            instance: &SourceInstance,
            object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            if self
                .dependency_free_bootstrap_panic_suffix
                .as_deref()
                .is_some_and(|suffix| object.relative_path.ends_with(suffix))
            {
                panic!("private fixture panic /Users/alice/private/session.jsonl");
            }
            if self
                .dependency_free_bootstrap_failure_suffix
                .as_deref()
                .is_some_and(|suffix| object.relative_path.ends_with(suffix))
            {
                return Err(AdapterError::new(
                    AdapterErrorClass::StreamFatal,
                    "fixture_bootstrap_failed",
                    "private fixture failure /Users/alice/private/session.jsonl",
                ));
            }
            if let Some(revision) = &self.dependency_free_context_revision {
                return AdapterObjectContext::new(
                    1,
                    revision.load(Ordering::Acquire).to_be_bytes().to_vec(),
                );
            }
            self.bootstrap_object(instance, object)
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
            if record.payload == b"stream-fatal" {
                return Err(AdapterError::new(
                    AdapterErrorClass::StreamFatal,
                    "fixture_stream_fatal",
                    "fixture stream-fatal decode",
                ));
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
            let dependency = if self.request_dependency_access {
                Some(source_access.read_object("root", std::path::Path::new("sidecar.json"), 16)?)
            } else {
                None
            };
            if let Some((path, payload)) = &self.dependency_mutation {
                std::fs::write(path, payload).map_err(|_| {
                    AdapterError::new(
                        AdapterErrorClass::AdapterFatal,
                        "fixture_dependency_mutation",
                        "fixture dependency mutation failed",
                    )
                })?;
            }
            let disposition = self.decode(context, record, output)?;
            if let Some(dependency) = dependency {
                let mut state = output.next_decoder_state().unwrap_or_default().to_vec();
                state.extend_from_slice(&dependency.revision.revision);
                output.set_next_decoder_state(state)?;
            }
            Ok(disposition)
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
        let catalog_authorization = registry
            .authorize_catalog_if_supported(&AdapterId::new("fixture").unwrap(), &probe)
            .unwrap()
            .unwrap();
        assert_eq!(
            catalog_authorization.operation().operation(),
            SupportOperation::CatalogDiscovery
        );
        let catalog_selection = catalog_authorization.select_catalog_access().unwrap();
        assert_eq!(catalog_selection.contracts().query_pack_version, Some(1));
        assert_eq!(
            catalog_selection.contracts().fact_family_versions,
            BTreeMap::from([
                ("catalog.project".to_string(), 1),
                ("catalog.session".to_string(), 1),
            ])
        );
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
    fn scoped_support_preparation_negotiates_before_probe_and_never_falls_back() {
        let (catalog, binding, scope_programs) = promoted_fixture_catalog();
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let registry = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("fixture").with_support(binding, scope_programs))
            .register_native_support_probe("fixture", {
                let probe_calls = Arc::clone(&probe_calls);
                move |_| {
                    probe_calls.fetch_add(1, Ordering::AcqRel);
                    Ok(NativeArtifactProbe {
                        family: "fixture".to_string(),
                        platform: "test".to_string(),
                        version: Some("1.0.0".to_string()),
                        markers: vec!["fixture.marker".to_string()],
                        contradictory_markers: false,
                    })
                }
            })
            .build_supported(catalog)
            .unwrap();
        let temp = TempDir::new().unwrap();
        let request = scoped_access_request(temp.path().to_path_buf());
        let mut incompatible = request.observation_contract_request.clone();
        incompatible.event_contract_versions = vec![2];
        assert!(matches!(
            prepare_scoped_observation_support(
                &registry,
                "fixture",
                &[temp.path().to_path_buf()],
                &incompatible,
                &request.observation_contract_offer,
                request.unknown_wire_contract.as_ref(),
            ),
            Err(ScopedObservationAccessError::ObservationContract(
                ObservationNegotiationError::IncompatibleObservationContract {
                    axis: ObservationCompatibilityAxis::EventContractVersion,
                }
            ))
        ));
        assert_eq!(probe_calls.load(Ordering::Acquire), 0);

        let incompatible_unknown_offer = serde_json::from_value(serde_json::json!({
            "observation_unknown_wire_negotiation_contract_version": 1,
            "capability": {
                "unknown_wire_event_contract_version": 1,
                "preserves_type_tag": false,
                "preserves_encoded_value": true,
                "preserves_envelope_provenance": true,
                "max_preserved_bytes": 4096
            }
        }))
        .unwrap();
        let incompatible_unknown = ScopedObservationUnknownWireNegotiation::new(
            ObservationUnknownWireContractRequest::new(
                ObservationUnknownWireCapability::preserving(8_192).unwrap(),
            )
            .unwrap(),
            incompatible_unknown_offer,
        );
        assert!(matches!(
            prepare_scoped_observation_support(
                &registry,
                "fixture",
                &[temp.path().to_path_buf()],
                &request.observation_contract_request,
                &request.observation_contract_offer,
                Some(&incompatible_unknown),
            ),
            Err(ScopedObservationAccessError::UnknownWireContract(
                ObservationUnknownWireContractError::Incompatible {
                    axis: ObservationUnknownWireCompatibilityAxis::TypeTagPreservation,
                }
            ))
        ));
        assert_eq!(probe_calls.load(Ordering::Acquire), 0);

        let prepared = prepare_scoped_observation_support(
            &registry,
            "fixture",
            &[temp.path().to_path_buf()],
            &request.observation_contract_request,
            &request.observation_contract_offer,
            request.unknown_wire_contract.as_ref(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(prepared.adapter_id().as_str(), "fixture");
        assert_eq!(prepared.artifact_probe().version.as_deref(), Some("1.0.0"));
        assert_eq!(
            prepared.observation_contract().contract_versions,
            *prepared.authorization().contracts()
        );
        assert!(prepared.unknown_wire_contract().is_some());
        assert!(prepared.compatibility().permissions().scoped_observation);
        let expected_selection = prepared.observation_contract().clone();
        let trusted_request = ScopedObservationTrustedAccessRequest::new(
            request.source_instance.clone(),
            request.artifact_access_policy,
            request.root_identity.clone(),
            request.program_id.clone(),
            request.known_objects.clone(),
            request.access_roots.clone(),
            request.artifact_relations.clone(),
        );
        let trusted_debug = format!("{trusted_request:?}");
        assert!(!trusted_debug.contains(temp.path().to_string_lossy().as_ref()));
        let host = ScopedObservationAccessHost::authorize_prepared(prepared, trusted_request)
            .expect("prepared support should open the matching trusted attachment");
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(host.contract_selection(), &expected_selection);
        assert_eq!(
            host.compatibility().support_release_id(),
            Some("fixture-release")
        );

        let (catalog, binding, scope_programs) = promoted_fixture_catalog();
        let unsupported = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("fixture").with_support(binding, scope_programs))
            .register_native_support_probe("fixture", |_| {
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("9.9.9".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap();
        assert!(prepare_scoped_observation_support(
            &unsupported,
            "fixture",
            &[temp.path().to_path_buf()],
            &request.observation_contract_request,
            &request.observation_contract_offer,
            request.unknown_wire_contract.as_ref(),
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn configured_scoped_attachment_binds_one_promoted_root_without_a_store() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_attachment_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
            "1.0.0",
        );
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("configured-private-root");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::write(root.join("sessions/session.jsonl"), b"fixture\n").unwrap();
        let request = configured_attachment_request(
            vec![root.clone()],
            PathBuf::from("sessions/session.jsonl"),
        );
        let request_debug = format!("{request:?}");
        assert!(!request_debug.contains("configured-private-root"));
        assert!(!request_debug.contains("fixture-session"));

        let attachment = prepare_configured_scoped_observation_attachment(&registry, request)
            .unwrap()
            .expect("the promoted configured root should compose");
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 1);
        assert_eq!(attachment.root_source().relation_id(), "root-object");
        assert_eq!(attachment.root_source().stream().id.as_str(), "root-stream");
        assert_eq!(attachment.known_object_sources().count(), 1);
        assert_eq!(
            attachment
                .relation_identity_inputs("root-object")
                .unwrap()
                .iter()
                .map(ScopedObservationOwnedIdentityInput::name)
                .collect::<Vec<_>>(),
            vec!["native-session-id"]
        );
        let rendered = format!("{attachment:?}");
        assert!(!rendered.contains("configured-private-root"));
        assert!(!rendered.contains("fixture-session"));

        let prepared_runtime = attachment.prepare_append_runtime(16, 16).unwrap();
        assert_eq!(prepared_runtime.objects().len(), 1);
        assert_eq!(prepared_runtime.bindings().len(), 1);
        assert_eq!(prepared_runtime.bindings()[0].relation_id(), "root-object");
        assert_eq!(
            prepared_runtime.objects()[0].source_identity().stream_key,
            CoverageStreamKey::derive("fixture", b"root-stream").unwrap()
        );
        assert_eq!(
            prepared_runtime.objects()[0].source_identity().object_key,
            CoverageObjectKey::derive(
                "root-stream",
                &confined_relative_path_key(std::path::Path::new("sessions/session.jsonl"))
                    .unwrap(),
            )
            .unwrap()
        );
        let rendered = format!("{prepared_runtime:?}");
        assert!(!rendered.contains("configured-private-root"));
        assert!(!rendered.contains("fixture-session"));

        let inputs = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"fixture-session",
        }];
        let pass = prepared_runtime.host().begin_pass().unwrap();
        assert!(matches!(
            pass.read_known_object(ScopedKnownObjectReadRequest {
                relation_id: "root-object",
                identity_inputs: &inputs,
                phase: AccessPhase::Initial,
                parent_token: None,
                depth: 1,
                max_bytes: 1_024,
            })
            .unwrap(),
            ScopedObjectRead::Available { .. }
        ));
        drop(pass);
        let (host, objects, bindings, directory_bindings, related_relation_bindings) =
            prepared_runtime.into_parts();
        assert_eq!(objects.len(), 1);
        assert_eq!(bindings.len(), 1);
        assert!(directory_bindings.is_empty());
        assert!(related_relation_bindings.is_empty());
        drop(host);
    }

    #[test]
    fn configured_related_object_identity_stays_evidence_derived_and_non_authorizing() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_related_object_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
        );
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("configured-related-private-root");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::write(root.join("sessions/session.jsonl"), b"fixture\n").unwrap();

        let attachment = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(
                vec![root.clone()],
                PathBuf::from("sessions/session.jsonl"),
            ),
        )
        .unwrap()
        .unwrap();
        assert!(attachment
            .relation_identity_inputs("team-config-from-evidence")
            .is_none());
        let related = attachment.related_relation_bindings().collect::<Vec<_>>();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].relation_id(), "team-config-from-evidence");
        assert_eq!(
            related[0].primitive(),
            ScopeRelationPrimitive::ReferencedObjectFromField
        );
        assert_eq!(
            related[0].identity_input_names().collect::<Vec<_>>(),
            vec!["team-name"]
        );
        assert_eq!(related[0].bounds().max_objects, 4);

        let prepared = attachment.prepare_append_runtime(16, 16).unwrap();
        assert_eq!(prepared.objects().len(), 1);
        assert!(prepared.directory_bindings().is_empty());
        assert_eq!(prepared.related_relation_bindings().len(), 1);
        assert_eq!(prepared.required_coverage_objects(), 5);

        let source_key = CanonicalSourceInstanceKey::derive(1, b"related-planner").unwrap();
        let join_update =
            |native_fact_key: &[u8], relation_id: &str, input_name: &str, input_value: &[u8]| {
                let fact_id = CanonicalFactId::native(
                    "fixture",
                    &source_key,
                    "runtime.actor-affiliation",
                    native_fact_key,
                )
                .unwrap();
                let semantic_revision_ref = SemanticRevisionRef::new(
                    FactRevisionId::derive(&fact_id, 1, b"related-planner-revision").unwrap(),
                );
                ScopeJoinUpdate::new(
                    relation_id,
                    vec![ScopeJoinEvidence::new(fact_id, semantic_revision_ref)],
                    vec![ScopeJoinParameterSet::new(vec![ScopeJoinIdentityInput::new(
                        input_name,
                        input_value.to_vec(),
                    )
                    .unwrap()])
                    .unwrap()],
                )
                .unwrap()
            };
        let snapshot = ScopedObservationScopeJoinSnapshot::from_updates_for_test(vec![
            join_update(
                b"first-owner",
                "team-config-from-evidence",
                "team-name",
                b"private-team-coordinate",
            ),
            join_update(
                b"second-owner",
                "team-config-from-evidence",
                "team-name",
                b"private-team-coordinate",
            ),
        ])
        .unwrap();
        let plan = prepared.plan_related_sources(&snapshot).unwrap();
        assert_eq!(
            plan.declared_relation_ids().collect::<Vec<_>>(),
            vec!["team-config-from-evidence"]
        );
        assert_eq!(plan.sources().len(), 1);
        assert_eq!(plan.sources()[0].relation_id(), "team-config-from-evidence");
        assert_eq!(
            plan.sources()[0].primitive(),
            ScopeRelationPrimitive::ReferencedObjectFromField
        );
        assert_eq!(
            plan.sources()[0].identity_input_names().collect::<Vec<_>>(),
            vec!["team-name"]
        );
        assert_eq!(plan.sources()[0].evidence_group_count(), 2);
        assert_eq!(plan.sources()[0].bounds().max_bytes, 4_096);
        assert_eq!(
            plan.sources()[0].borrowed_identity_inputs()[0].value,
            b"private-team-coordinate"
        );
        assert_eq!(plan.snapshot_retained_bytes(), snapshot.retained_bytes());
        let plan_debug = format!("{plan:?} {:?}", plan.sources()[0]);
        assert!(!plan_debug.contains("private-team-coordinate"));

        let wrong_input =
            ScopedObservationScopeJoinSnapshot::from_updates_for_test(vec![join_update(
                b"wrong-input-owner",
                "team-config-from-evidence",
                "wrong-name",
                b"private-team-coordinate",
            )])
            .unwrap();
        assert!(matches!(
            prepared.plan_related_sources(&wrong_input),
            Err(crate::scoped_observation::configured_attachment::ConfiguredScopedObservationRuntimeError::SourceBinding)
        ));
        let undeclared =
            ScopedObservationScopeJoinSnapshot::from_updates_for_test(vec![join_update(
                b"undeclared-owner",
                "undeclared-relation",
                "team-name",
                b"private-team-coordinate",
            )])
            .unwrap();
        assert!(matches!(
            prepared.plan_related_sources(&undeclared),
            Err(crate::scoped_observation::configured_attachment::ConfiguredScopedObservationRuntimeError::SourceBinding)
        ));

        let fan_out_fact = CanonicalFactId::native(
            "fixture",
            &source_key,
            "runtime.actor-affiliation",
            b"fan-out-owner",
        )
        .unwrap();
        let fan_out_revision = SemanticRevisionRef::new(
            FactRevisionId::derive(&fan_out_fact, 1, b"fan-out-revision").unwrap(),
        );
        let fan_out = ScopeJoinUpdate::new(
            "team-config-from-evidence",
            vec![ScopeJoinEvidence::new(fan_out_fact, fan_out_revision)],
            (0_u8..5)
                .map(|index| {
                    ScopeJoinParameterSet::new(vec![ScopeJoinIdentityInput::new(
                        "team-name",
                        vec![b't', index],
                    )
                    .unwrap()])
                    .unwrap()
                })
                .collect(),
        )
        .unwrap();
        let fan_out =
            ScopedObservationScopeJoinSnapshot::from_updates_for_test(vec![fan_out]).unwrap();
        assert!(matches!(
            prepared.plan_related_sources(&fan_out),
            Err(crate::scoped_observation::configured_attachment::ConfiguredScopedObservationRuntimeError::SourceBinding)
        ));

        let too_many_objects = ScopedObservationScopeJoinSnapshot::from_updates_for_test(
            (0_u8..5)
                .map(|index| {
                    join_update(
                        &[b'o', index],
                        "team-config-from-evidence",
                        "team-name",
                        &[b't', index],
                    )
                })
                .collect(),
        )
        .unwrap();
        assert!(matches!(
            prepared.plan_related_sources(&too_many_objects),
            Err(crate::scoped_observation::configured_attachment::ConfiguredScopedObservationRuntimeError::SourceBinding)
        ));

        let factory_called = Arc::new(AtomicBool::new(false));
        let error = prepared
            .open_with_watcher_factory(ConfiguredScopedObservationRuntimeOptions::default(), {
                let factory_called = Arc::clone(&factory_called);
                move |_| {
                    factory_called.store(true, Ordering::Release);
                    unreachable!("related-object runtime must fail before watcher installation")
                }
            })
            .unwrap_err();
        assert_eq!(
            error,
            crate::scoped_observation::configured_attachment::ConfiguredScopedObservationRuntimeError::SourceBinding
        );
        assert!(!factory_called.load(Ordering::Acquire));

        let injected_identity = ScopedConfiguredRootIdentity::new(
            b"fixture-session".as_slice(),
            BTreeMap::from([
                (
                    "native-session-id".to_string(),
                    Arc::<[u8]>::from(b"fixture-session".as_slice()),
                ),
                (
                    "team-name".to_string(),
                    Arc::<[u8]>::from(b"caller-injected-private-team".as_slice()),
                ),
            ]),
        )
        .unwrap()
        .with_root_run_identity_key(Arc::from(b"fixture-root-run".as_slice()));
        let template = scoped_access_request(root.clone());
        let injected_request = ScopedConfiguredAttachmentRequest::new(
            "fixture",
            vec![root],
            "observe-session",
            BTreeMap::from([(
                "root-object".to_string(),
                PathBuf::from("sessions/session.jsonl"),
            )]),
            injected_identity,
            template.observation_contract_request,
            template.observation_contract_offer,
        )
        .unwrap()
        .with_unknown_wire_contract(template.unknown_wire_contract.unwrap());
        let error = prepare_configured_scoped_observation_attachment(&registry, injected_request)
            .unwrap_err();
        assert!(!error.to_string().contains("caller-injected-private-team"));
        assert_eq!(probe_calls.load(Ordering::Acquire), 2);
        assert_eq!(discover_calls.load(Ordering::Acquire), 1);
    }

    #[cfg(unix)]
    #[test]
    fn configured_related_reconciliation_is_atomic_owner_bound_and_path_free() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_related_object_registry(probe_calls, discover_calls);
        let temp = TempDir::new().unwrap();
        let root = temp
            .path()
            .join("configured-related-reconciliation-private-root");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::create_dir_all(root.join("teams/private-team-coordinate")).unwrap();
        std::fs::write(root.join("sessions/session.jsonl"), b"root\n").unwrap();
        let initial_payload = b"initial-private-related-document";
        std::fs::write(
            root.join("teams/private-team-coordinate/config.json"),
            initial_payload,
        )
        .unwrap();

        let prepared = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(
                vec![root.clone()],
                PathBuf::from("sessions/session.jsonl"),
            ),
        )
        .unwrap()
        .unwrap()
        .prepare_append_runtime(16, 16)
        .unwrap();
        let source_key = CanonicalSourceInstanceKey::derive(1, b"related-reconciler").unwrap();
        let join_update = |native_fact_key: &[u8], team_name: &[u8]| {
            let fact_id = CanonicalFactId::native(
                "fixture",
                &source_key,
                "runtime.actor-affiliation",
                native_fact_key,
            )
            .unwrap();
            ScopeJoinUpdate::new(
                "team-config-from-evidence",
                vec![ScopeJoinEvidence::new(
                    fact_id,
                    SemanticRevisionRef::new(
                        FactRevisionId::derive(&fact_id, 1, b"related-reconciler-revision")
                            .unwrap(),
                    ),
                )],
                vec![ScopeJoinParameterSet::new(vec![ScopeJoinIdentityInput::new(
                    "team-name",
                    team_name.to_vec(),
                )
                .unwrap()])
                .unwrap()],
            )
            .unwrap()
        };
        let snapshot = ScopedObservationScopeJoinSnapshot::from_updates_for_test(vec![
            join_update(b"first-owner", b"private-team-coordinate"),
            join_update(b"second-owner", b"private-team-coordinate"),
        ])
        .unwrap();
        let expected_token = AccessObjectToken::derive(
            "team-config-from-evidence",
            &[
                b"team-name".as_slice(),
                b"private-team-coordinate".as_slice(),
            ],
        )
        .unwrap();

        let pass = prepared.host().begin_pass().unwrap();
        let initial_pass_id = pass.pass_id();
        let initial = prepared
            .execute_related_sources(
                &pass,
                prepared.plan_related_sources(&snapshot).unwrap(),
                None,
                AccessPhase::Initial,
                100,
            )
            .unwrap();
        let initial_report = pass.finish();
        assert_eq!(initial.observations().len(), 1);
        assert_eq!(initial.observations()[0].object_token(), expected_token);
        let PreparedScopedRelatedSourceObservation::Initial(observation) =
            initial.observations()[0].observation()
        else {
            panic!("the first related reconciliation must retain an initial observation")
        };
        let decoded = observation.present_snapshot().unwrap();
        assert_eq!(decoded.generation(), 1);
        assert_eq!(decoded.revision(), Revision::digest(initial_payload));
        assert_eq!(initial.next_state().sources().len(), 1);
        assert_eq!(initial.memberships().len(), 1);
        assert_eq!(
            initial.memberships()[0].relation_id(),
            "team-config-from-evidence"
        );
        assert_eq!(initial.memberships()[0].access_pass_id(), initial_pass_id);
        assert_eq!(initial.memberships()[0].member_sources().count(), 1);
        assert_ne!(
            initial.memberships()[0].source(),
            decoded.identity().source()
        );
        let initial_membership_revision = initial.memberships()[0].revision();
        assert_eq!(
            initial
                .next_state()
                .sources()
                .get(&expected_token)
                .unwrap()
                .binding()
                .evidence_group_count(),
            2
        );
        assert_eq!(
            initial.next_state().snapshot_retained_bytes(),
            snapshot.retained_bytes()
        );
        assert_eq!(
            initial
                .next_state()
                .declared_relation_ids()
                .collect::<Vec<_>>(),
            vec!["team-config-from-evidence"]
        );
        let initial_relation = initial_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(initial_relation.attempts, 1);
        assert_eq!(initial_relation.completed, 1);
        assert_eq!(initial_relation.bytes_read, initial_payload.len() as u64);

        let corrected_payload = b"corrected-private-related-document";
        std::fs::write(
            root.join("teams/private-team-coordinate/config.json"),
            corrected_payload,
        )
        .unwrap();
        let corrected_join_snapshot = ScopedObservationScopeJoinSnapshot::from_updates_for_test(
            vec![join_update(b"second-owner", b"private-team-coordinate")],
        )
        .unwrap();
        let pass = prepared.host().begin_pass().unwrap();
        let corrected = prepared
            .execute_related_sources(
                &pass,
                prepared
                    .plan_related_sources(&corrected_join_snapshot)
                    .unwrap(),
                Some(initial.next_state()),
                AccessPhase::Revalidation,
                200,
            )
            .unwrap();
        let corrected_report = pass.finish();
        let PreparedScopedRelatedSourceObservation::Refresh(observation) =
            corrected.observations()[0].observation()
        else {
            panic!("the second related reconciliation must retain a refresh observation")
        };
        let decoded = observation.present_snapshot_for_test().unwrap();
        assert_eq!(decoded.generation(), 1);
        assert_eq!(decoded.revision(), Revision::digest(corrected_payload));
        assert_eq!(
            corrected
                .next_state()
                .sources()
                .get(&expected_token)
                .unwrap()
                .state()
                .decoder_state_for_test(),
            Some(
                [initial_payload.as_slice(), corrected_payload.as_slice()]
                    .concat()
                    .as_slice()
            )
        );
        assert_eq!(
            corrected_report
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "team-config-from-evidence")
                .unwrap()
                .bytes_read,
            corrected_payload.len() as u64
        );
        assert_eq!(
            corrected
                .next_state()
                .sources()
                .get(&expected_token)
                .unwrap()
                .binding()
                .evidence_group_count(),
            1
        );
        assert_ne!(
            corrected.memberships()[0].revision(),
            initial_membership_revision
        );
        assert_eq!(
            initial
                .next_state()
                .sources()
                .get(&expected_token)
                .unwrap()
                .binding()
                .evidence_group_count(),
            2
        );

        std::fs::remove_file(root.join("teams/private-team-coordinate/config.json")).unwrap();
        let pass = prepared.host().begin_pass().unwrap();
        let removed = prepared
            .execute_related_sources(
                &pass,
                prepared
                    .plan_related_sources(&corrected_join_snapshot)
                    .unwrap(),
                Some(corrected.next_state()),
                AccessPhase::Revalidation,
                250,
            )
            .unwrap();
        let removed_report = pass.finish();
        let PreparedScopedRelatedSourceObservation::Refresh(observation) =
            removed.observations()[0].observation()
        else {
            panic!("the missing related source must retain a refresh observation")
        };
        let decoded = observation.removed_snapshot_for_test().unwrap();
        assert_eq!(decoded.generation(), 2);
        assert_eq!(removed.memberships().len(), 1);
        assert_eq!(
            removed.memberships()[0].revision(),
            corrected.memberships()[0].revision()
        );
        let removed_relation = removed_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(removed_relation.attempts, 1);
        assert_eq!(removed_relation.completed, 1);
        assert_eq!(removed_relation.bytes_read, 0);

        let empty_snapshot =
            ScopedObservationScopeJoinSnapshot::from_updates_for_test(Vec::new()).unwrap();
        let pass = prepared.host().begin_pass().unwrap();
        let retired = prepared
            .execute_related_sources(
                &pass,
                prepared.plan_related_sources(&empty_snapshot).unwrap(),
                Some(removed.next_state()),
                AccessPhase::Revalidation,
                300,
            )
            .unwrap();
        let retired_report = pass.finish();
        assert!(retired.observations().is_empty());
        assert!(retired.next_state().sources().is_empty());
        assert_eq!(retired.memberships().len(), 1);
        assert_eq!(retired.memberships()[0].member_sources().count(), 0);
        assert_ne!(
            retired.memberships()[0].revision(),
            removed.memberships()[0].revision()
        );
        assert_eq!(retired.retired_sources().len(), 1);
        assert_eq!(
            retired.retired_sources()[0]
                .binding()
                .evidence_group_count(),
            1
        );
        assert_eq!(
            retired_report
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "team-config-from-evidence")
                .unwrap()
                .attempts,
            0
        );

        let foreign = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(
                vec![root.clone()],
                PathBuf::from("sessions/session.jsonl"),
            ),
        )
        .unwrap()
        .unwrap()
        .prepare_append_runtime(16, 16)
        .unwrap();
        let foreign_plan = foreign.plan_related_sources(&snapshot).unwrap();
        let pass = prepared.host().begin_pass().unwrap();
        assert!(matches!(
            prepared.execute_related_sources(
                &pass,
                foreign_plan,
                None,
                AccessPhase::Revalidation,
                400,
            ),
            Err(crate::scoped_observation::configured_attachment::ConfiguredScopedObservationRuntimeError::SourceBinding)
        ));
        let foreign_report = pass.finish();
        assert_eq!(
            foreign_report
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "team-config-from-evidence")
                .unwrap()
                .attempts,
            0
        );

        let rendered = format!("{initial:?} {corrected:?} {removed:?} {retired:?}");
        for private in [
            "configured-related-reconciliation-private-root",
            "private-team-coordinate",
            "initial-private-related-document",
            "corrected-private-related-document",
            "config.json",
        ] {
            assert!(!rendered.contains(private));
        }

        let (_observations, mut memberships, retired_sources, next_state) = retired.into_parts();
        assert_eq!(memberships.len(), 1);
        assert_eq!(retired_sources.len(), 1);
        assert!(next_state.sources().is_empty());
        let authority = memberships.pop().unwrap();
        let membership_source = authority.source().clone();
        let membership_pass_id = authority.access_pass_id();
        assert!(membership_pass_id > 0);
        let membership =
            ScopedRelationMembershipObservation::from_related_membership(authority).unwrap();
        let mut admission = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events: 8,
            max_retained_native_bytes: 8_192,
            max_control_items: 8,
            max_coverage_objects: 2,
        })
        .unwrap();
        admission
            .record_related_relation_membership(
                prepared.host(),
                ScopedAppendDeliveryPhase::Correction,
                membership,
            )
            .unwrap();
        assert!(admission
            .directory_relation_listing("team-config-from-evidence")
            .is_none());
        let membership_coverage = admission
            .offered_decode_coverage(&membership_source)
            .unwrap();
        assert_eq!(
            membership_coverage.point.as_ref().unwrap().status,
            CoverageStatus::ExactSnapshot
        );
        assert_eq!(
            membership_coverage.completeness,
            CoverageSetCompleteness::Complete
        );

        let foreign_pass = foreign.host().begin_pass().unwrap();
        let foreign_batch = foreign
            .execute_related_sources(
                &foreign_pass,
                foreign.plan_related_sources(&snapshot).unwrap(),
                None,
                AccessPhase::Revalidation,
                500,
            )
            .unwrap();
        foreign_pass.finish();
        let (_, mut foreign_memberships, _, _) = foreign_batch.into_parts();
        let foreign_authority = foreign_memberships.pop().unwrap();
        let foreign_membership_source = foreign_authority.source().clone();
        let foreign_membership =
            ScopedRelationMembershipObservation::from_related_membership(foreign_authority)
                .unwrap();
        let mut foreign_admission =
            ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
                max_data_events: 8,
                max_retained_native_bytes: 8_192,
                max_control_items: 8,
                max_coverage_objects: 3,
            })
            .unwrap();
        assert_eq!(
            foreign_admission.record_related_relation_membership(
                prepared.host(),
                ScopedAppendDeliveryPhase::Correction,
                foreign_membership,
            ),
            Err(ScopedAdmissionError::InvalidCoverage)
        );
        assert!(foreign_admission
            .offered_decode_coverage(&foreign_membership_source)
            .is_none());

        let (mut observations, mut memberships, retired_sources, next_state) = initial.into_parts();
        assert_eq!(observations.len(), 1);
        assert_eq!(memberships.len(), 1);
        assert!(retired_sources.is_empty());
        assert_eq!(next_state.sources().len(), 1);
        let observed = observations.pop().unwrap();
        assert_eq!(observed.object_token(), expected_token);
        let (_, observation) = observed.into_parts();
        let authority = memberships.pop().unwrap();
        let member_source = authority.member_sources().next().unwrap().clone();
        let membership_source = authority.source().clone();
        let related_pass_id = authority.access_pass_id();
        let membership =
            ScopedRelationMembershipObservation::from_related_membership(authority).unwrap();
        let mut related_admission =
            ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
                max_data_events: 8,
                max_retained_native_bytes: 8_192,
                max_control_items: 8,
                max_coverage_objects: 2,
            })
            .unwrap();
        related_admission
            .record_related_relation_membership(
                prepared.host(),
                ScopedAppendDeliveryPhase::Bootstrap,
                membership,
            )
            .unwrap();
        let failure = related_admission
            .admit_related_object(
                related_pass_id + 1,
                ScopedAppendDeliveryPhase::Bootstrap,
                observation,
            )
            .unwrap_err();
        assert_eq!(failure.error, ScopedAdmissionError::InvalidCoverage);
        let rendered = format!("{failure:?}");
        for private in [
            "configured-related-reconciliation-private-root",
            "private-team-coordinate",
            "initial-private-related-document",
            "config.json",
        ] {
            assert!(!rendered.contains(private));
        }
        assert_eq!(related_admission.queued_data_events(), 0);
        assert!(related_admission
            .offered_decode_coverage(&member_source)
            .is_none());
        let receipt = related_admission
            .admit_related_object(
                related_pass_id,
                ScopedAppendDeliveryPhase::Bootstrap,
                *failure.observation,
            )
            .unwrap();
        assert_eq!(receipt.data_events, 1);
        assert_eq!(receipt.control_items, 0);
        assert!(related_admission
            .offered_decode_coverage(&membership_source)
            .is_some());
        assert!(related_admission
            .offered_decode_coverage(&member_source)
            .is_none());
        match related_admission.pop_next() {
            Some(ScopedQueuedObservationFrame::Decoded { item, source, .. }) => {
                assert_eq!(source, member_source);
                match *item {
                    ScopedDecodedAppendItem::Record {
                        disposition, batch, ..
                    } => {
                        assert_eq!(disposition, DecodeDisposition::PreservedUnknown);
                        assert_eq!(batch.facts().len(), 1);
                    }
                    ScopedDecodedAppendItem::DriverQuarantine(_) => {
                        panic!("expected one admitted related record")
                    }
                }
            }
            Some(_) | None => panic!("expected one admitted related decoded frame"),
        }
        let member_coverage = related_admission
            .offered_decode_coverage(&member_source)
            .unwrap();
        assert_eq!(
            member_coverage.point.as_ref().unwrap().status,
            CoverageStatus::ExactSnapshot
        );
        assert_eq!(
            member_coverage.completeness,
            CoverageSetCompleteness::Complete
        );
        assert!(related_admission.pop_next().is_none());

        let (mut observations, mut memberships, retired_sources, next_state) =
            corrected.into_parts();
        assert_eq!(observations.len(), 1);
        assert_eq!(memberships.len(), 1);
        assert!(retired_sources.is_empty());
        assert_eq!(next_state.sources().len(), 1);
        let (_, observation) = observations.pop().unwrap().into_parts();
        let authority = memberships.pop().unwrap();
        assert_eq!(authority.member_sources().next(), Some(&member_source));
        let corrected_pass_id = authority.access_pass_id();
        related_admission
            .record_related_relation_membership(
                prepared.host(),
                ScopedAppendDeliveryPhase::Correction,
                ScopedRelationMembershipObservation::from_related_membership(authority).unwrap(),
            )
            .unwrap();
        let receipt = related_admission
            .admit_related_object(
                corrected_pass_id,
                ScopedAppendDeliveryPhase::Correction,
                observation,
            )
            .unwrap();
        assert_eq!(receipt.data_events, 1);
        match related_admission.pop_next() {
            Some(ScopedQueuedObservationFrame::Decoded { item, source, .. }) => {
                assert_eq!(source, member_source);
                match *item {
                    ScopedDecodedAppendItem::Record { evidence, .. } => {
                        assert_eq!(evidence.state, SourceRecordState::Present);
                        assert_eq!(evidence.payload_hash, RecordHash::digest(corrected_payload));
                    }
                    ScopedDecodedAppendItem::DriverQuarantine(_) => {
                        panic!("expected one admitted related correction")
                    }
                }
            }
            Some(_) | None => panic!("expected one admitted related correction frame"),
        }
        assert_eq!(
            related_admission
                .offered_decode_coverage(&member_source)
                .unwrap()
                .point
                .as_ref()
                .unwrap()
                .status,
            CoverageStatus::ExactSnapshot
        );

        let (mut observations, mut memberships, retired_sources, next_state) = removed.into_parts();
        assert_eq!(observations.len(), 1);
        assert_eq!(memberships.len(), 1);
        assert!(retired_sources.is_empty());
        assert_eq!(next_state.sources().len(), 1);
        let (_, observation) = observations.pop().unwrap().into_parts();
        let authority = memberships.pop().unwrap();
        assert_eq!(authority.member_sources().next(), Some(&member_source));
        let removed_pass_id = authority.access_pass_id();
        related_admission
            .record_related_relation_membership(
                prepared.host(),
                ScopedAppendDeliveryPhase::Correction,
                ScopedRelationMembershipObservation::from_related_membership(authority).unwrap(),
            )
            .unwrap();
        let receipt = related_admission
            .admit_related_object(
                removed_pass_id,
                ScopedAppendDeliveryPhase::Correction,
                observation,
            )
            .unwrap();
        assert_eq!(receipt.data_events, 1);
        match related_admission.pop_next() {
            Some(ScopedQueuedObservationFrame::Decoded { item, source, .. }) => {
                assert_eq!(source, member_source);
                match *item {
                    ScopedDecodedAppendItem::Record { evidence, .. } => {
                        assert_eq!(evidence.state, SourceRecordState::Absent);
                    }
                    ScopedDecodedAppendItem::DriverQuarantine(_) => {
                        panic!("expected one admitted related removal")
                    }
                }
            }
            Some(_) | None => panic!("expected one admitted related removal frame"),
        }
        let member_coverage = related_admission
            .offered_decode_coverage(&member_source)
            .unwrap();
        assert!(member_coverage.point.is_none());
        assert_eq!(
            member_coverage.explicit_absence_or_deletion,
            Some(crate::adapter::CoverageAbsence {
                stream_key: member_source.stream_key,
                object_key: member_source.object_key,
                generation: 2,
                kind: CoverageAbsenceKind::Deleted,
            })
        );
        assert_eq!(
            member_coverage.completeness,
            CoverageSetCompleteness::Complete
        );
        assert!(related_admission.pop_next().is_none());

        std::fs::create_dir_all(root.join("teams/oversized-team")).unwrap();
        std::fs::write(
            root.join("teams/oversized-team/config.json"),
            vec![b'x'; 4_097],
        )
        .unwrap();
        for (team_name, native_fact_key, expect_oversized) in [
            ("missing-team", b"missing-owner".as_slice(), false),
            ("oversized-team", b"oversized-owner".as_slice(), true),
        ] {
            let snapshot =
                ScopedObservationScopeJoinSnapshot::from_updates_for_test(vec![join_update(
                    native_fact_key,
                    team_name.as_bytes(),
                )])
                .unwrap();
            let pass = prepared.host().begin_pass().unwrap();
            let batch = prepared
                .execute_related_sources(
                    &pass,
                    prepared.plan_related_sources(&snapshot).unwrap(),
                    None,
                    AccessPhase::Initial,
                    600,
                )
                .unwrap();
            pass.finish();
            let (mut observations, mut memberships, retired_sources, next_state) =
                batch.into_parts();
            assert_eq!(observations.len(), 1);
            assert_eq!(memberships.len(), 1);
            assert!(retired_sources.is_empty());
            assert_eq!(next_state.sources().len(), 1);
            let (_, observation) = observations.pop().unwrap().into_parts();
            let authority = memberships.pop().unwrap();
            let member_source = authority.member_sources().next().unwrap().clone();
            let pass_id = authority.access_pass_id();
            let mut admission = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
                max_data_events: 1,
                max_retained_native_bytes: 1,
                max_control_items: 1,
                max_coverage_objects: 2,
            })
            .unwrap();
            admission
                .record_related_relation_membership(
                    prepared.host(),
                    ScopedAppendDeliveryPhase::Bootstrap,
                    ScopedRelationMembershipObservation::from_related_membership(authority)
                        .unwrap(),
                )
                .unwrap();
            let receipt = admission
                .admit_related_object(pass_id, ScopedAppendDeliveryPhase::Bootstrap, observation)
                .unwrap();
            assert_eq!(receipt.data_events, 0);
            assert_eq!(receipt.retained_native_bytes, 0);
            assert_eq!(receipt.control_items, 0);
            assert!(admission.pop_next().is_none());
            let coverage = admission.offered_decode_coverage(&member_source).unwrap();
            if expect_oversized {
                assert!(coverage.explicit_absence_or_deletion.is_none());
                assert!(matches!(
                    coverage.point.as_ref().map(|point| &point.status),
                    Some(CoverageStatus::Unavailable { reason }) if reason == "oversized"
                ));
                assert_eq!(coverage.explicit_errors.len(), 1);
                assert_eq!(coverage.explicit_errors[0].code, "oversized");
                assert_eq!(coverage.completeness, CoverageSetCompleteness::Unavailable);
            } else {
                assert!(coverage.point.is_none());
                assert_eq!(
                    coverage
                        .explicit_absence_or_deletion
                        .as_ref()
                        .map(|absence| (absence.generation, absence.kind)),
                    Some((1, CoverageAbsenceKind::Absent))
                );
                assert!(coverage.explicit_errors.is_empty());
                assert_eq!(coverage.completeness, CoverageSetCompleteness::Complete);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn authorized_related_object_initial_read_is_confined_accounted_and_decoded() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_related_object_registry(probe_calls, discover_calls);
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("related-private-root");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::create_dir_all(root.join("teams/private-team-coordinate")).unwrap();
        std::fs::create_dir_all(root.join("teams/oversized-team")).unwrap();
        std::fs::write(root.join("sessions/session.jsonl"), b"root\n").unwrap();
        let related_payload = b"related-private-document";
        std::fs::write(
            root.join("teams/private-team-coordinate/config.json"),
            related_payload,
        )
        .unwrap();
        std::fs::write(
            root.join("teams/oversized-team/config.json"),
            vec![b'x'; 4_097],
        )
        .unwrap();

        let request = scoped_access_request(root.clone());
        let instance = request.source_instance.clone();
        let expected_source_instance = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        )
        .unwrap();
        let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
        let origin = RecordOrigin {
            source_instance_id: instance.id,
            stream_id: 71,
            object_id: 72,
            observed_at: 73,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        };
        let observe = |team_name: &[u8], phase: AccessPhase| {
            let pass = host.begin_pass().unwrap();
            let identity = [ScopeIdentityInput {
                name: "team-name",
                value: team_name,
            }];
            let observation = pass
                .reserve_observation_runtime_source(ScopeAccessRequest {
                    relation_id: "team-config-from-evidence",
                    operation: AccessOperation::ObjectRead,
                    phase,
                    parent_token: None,
                    identity_inputs: &identity,
                    depth: 1,
                    max_bytes: 4_096,
                    max_rows: 0,
                })
                .unwrap()
                .observe_initial_related_replace(&origin)
                .unwrap();
            let report = pass.finish();
            (observation, report)
        };

        let (present, present_report) = observe(b"private-team-coordinate", AccessPhase::Initial);
        let snapshot = present.present_snapshot().unwrap();
        let expected_relative = PathBuf::from("teams/private-team-coordinate/config.json");
        let expected_object_key = confined_relative_path_key(&expected_relative).unwrap();
        let expected_stream_key =
            CoverageStreamKey::derive("fixture", b"team-config-stream").unwrap();
        let expected_coverage_object =
            CoverageObjectKey::derive("team-config-stream", &expected_object_key).unwrap();
        assert_eq!(
            snapshot.identity().relation_id(),
            "team-config-from-evidence"
        );
        assert_eq!(
            snapshot.identity().primitive(),
            ScopeRelationPrimitive::ReferencedObjectFromField
        );
        assert_eq!(
            snapshot.identity().object_token(),
            AccessObjectToken::derive(
                "team-config-from-evidence",
                &[
                    b"team-name".as_slice(),
                    b"private-team-coordinate".as_slice()
                ],
            )
            .unwrap()
        );
        assert_eq!(snapshot.identity().source().adapter_id.as_str(), "fixture");
        assert_eq!(
            snapshot.identity().source().source_instance_key,
            expected_source_instance
        );
        assert_eq!(snapshot.identity().source().stream_key, expected_stream_key);
        assert_eq!(
            snapshot.identity().source().object_key,
            expected_coverage_object
        );
        assert_eq!(
            snapshot.identity().semantic_context(),
            &FactSemanticContext::new(
                &AdapterId::new("fixture").unwrap(),
                instance.spec.identity_contract_version,
                instance.spec.stable_key.as_bytes(),
                b"team-config-stream",
                &expected_object_key,
                1,
            )
            .unwrap()
        );
        assert_eq!(snapshot.generation(), 1);
        assert_eq!(snapshot.revision(), Revision::digest(related_payload));
        assert_eq!(
            snapshot.disposition_for_test(),
            DecodeDisposition::PreservedUnknown
        );
        assert!(matches!(
            snapshot.mapping_disposition_for_test(),
            crate::adapter::RecordMappingDisposition::RetainedUnknown { .. }
        ));
        assert_eq!(snapshot.facts_for_test().len(), 1);
        assert!(matches!(
            &snapshot.facts_for_test()[0].value,
            Fact::UnknownRecord { raw_payload, .. } if raw_payload.is_empty()
        ));
        assert_eq!(
            snapshot.next_decoder_state_for_test(),
            Some(related_payload.as_slice())
        );
        assert_eq!(snapshot.record_for_test().payload, related_payload);
        assert_eq!(snapshot.admission_measurement(), Some((1, 0)));
        assert_eq!(
            snapshot.binding_for_test().runtime_stream_for_test(),
            &fixture_related_runtime_stream()
        );
        assert_eq!(
            snapshot.binding_for_test().source_instance_for_test().id,
            instance.id
        );
        assert_eq!(
            snapshot
                .binding_for_test()
                .descriptor_for_test()
                .relative_path,
            expected_relative
        );
        let rendered = format!("{present:?} {:?}", snapshot.identity());
        for private in [
            "private-team-coordinate",
            "related-private-document",
            "config.json",
            "related-private-root",
        ] {
            assert!(!rendered.contains(private));
        }
        let present_relation = present_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(present_relation.attempts, 1);
        assert_eq!(present_relation.completed, 1);
        assert_eq!(present_relation.bytes_read, related_payload.len() as u64);
        assert_eq!(present_relation.trace[0].outcome, AccessOutcome::Available);

        let refresh = |team_name: &[u8],
                       previous: &ScopedObservationRelatedObjectState,
                       phase: AccessPhase| {
            let pass = host.begin_pass().unwrap();
            let identity = [ScopeIdentityInput {
                name: "team-name",
                value: team_name,
            }];
            let observation = pass
                .reserve_observation_runtime_source(ScopeAccessRequest {
                    relation_id: "team-config-from-evidence",
                    operation: AccessOperation::ObjectRead,
                    phase,
                    parent_token: None,
                    identity_inputs: &identity,
                    depth: 1,
                    max_bytes: 4_096,
                    max_rows: 0,
                })
                .unwrap()
                .observe_related_replace_refresh(previous, &origin);
            let report = pass.finish();
            (observation, report)
        };
        let initial_state = present.refresh_state().unwrap();
        assert_eq!(
            initial_state.checkpoint_for_test().unwrap().revision,
            Revision::digest(related_payload)
        );
        assert_eq!(
            initial_state.decoder_state_for_test(),
            Some(related_payload.as_slice())
        );

        let corrected_payload = b"corrected-private-document";
        std::fs::write(
            root.join("teams/private-team-coordinate/config.json"),
            corrected_payload,
        )
        .unwrap();
        let (corrected, corrected_report) = refresh(
            b"private-team-coordinate",
            &initial_state,
            AccessPhase::Revalidation,
        );
        let corrected = corrected.unwrap();
        let corrected_snapshot = corrected.present_snapshot_for_test().unwrap();
        assert_eq!(corrected_snapshot.generation(), 1);
        assert_eq!(
            corrected_snapshot.revision(),
            Revision::digest(corrected_payload)
        );
        let expected_decoder_state =
            [related_payload.as_slice(), corrected_payload.as_slice()].concat();
        assert_eq!(
            corrected_snapshot.next_decoder_state_for_test(),
            Some(expected_decoder_state.as_slice())
        );
        let corrected_relation = corrected_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(
            corrected_relation.bytes_read,
            corrected_payload.len() as u64
        );
        assert_eq!(
            corrected_relation.trace[0].outcome,
            AccessOutcome::Available
        );
        let corrected_state = corrected.refresh_state().unwrap();

        let (unchanged, _) = refresh(
            b"private-team-coordinate",
            &corrected_state,
            AccessPhase::Revalidation,
        );
        let unchanged = unchanged.unwrap();
        assert!(unchanged.is_unchanged_for_test());
        let unchanged_state = unchanged.refresh_state().unwrap();
        assert_eq!(
            unchanged_state.decoder_state_for_test(),
            Some(expected_decoder_state.as_slice())
        );

        std::fs::remove_file(root.join("teams/private-team-coordinate/config.json")).unwrap();
        let (removed, removed_report) = refresh(
            b"private-team-coordinate",
            &unchanged_state,
            AccessPhase::Revalidation,
        );
        let removed = removed.unwrap();
        let removed_snapshot = removed.removed_snapshot_for_test().unwrap();
        assert_eq!(removed_snapshot.generation(), 2);
        assert_eq!(
            removed_snapshot.record_for_test().state,
            SourceRecordState::Absent
        );
        assert_eq!(removed_snapshot.record_for_test().payload, b"");
        assert_eq!(
            removed_report
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "team-config-from-evidence")
                .unwrap()
                .bytes_read,
            0
        );
        let removed_state = removed.refresh_state().unwrap();
        assert!(!removed_state.checkpoint_for_test().unwrap().present);

        let recreated_payload = b"recreated-private-document";
        std::fs::write(
            root.join("teams/private-team-coordinate/config.json"),
            recreated_payload,
        )
        .unwrap();
        let (recreated, _) = refresh(
            b"private-team-coordinate",
            &removed_state,
            AccessPhase::Revalidation,
        );
        let recreated = recreated.unwrap();
        let recreated_snapshot = recreated.present_snapshot_for_test().unwrap();
        assert_eq!(recreated_snapshot.generation(), 3);
        assert_eq!(
            recreated_snapshot.next_decoder_state_for_test(),
            Some(recreated_payload.as_slice())
        );

        let (substitution, substitution_report) = refresh(
            b"different-private-team",
            &removed_state,
            AccessPhase::Revalidation,
        );
        let substitution = substitution.unwrap_err();
        assert_eq!(
            substitution.to_string(),
            "scoped observation source binding does not match the active attachment"
        );
        assert!(!format!("{substitution:?}").contains("different-private-team"));
        let substitution_relation = substitution_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(
            substitution_relation.trace[0].outcome,
            AccessOutcome::Failed
        );

        let foreign_host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let foreign_pass = foreign_host.begin_pass().unwrap();
        let foreign_identity = [ScopeIdentityInput {
            name: "team-name",
            value: b"private-team-coordinate",
        }];
        let foreign_error = foreign_pass
            .reserve_observation_runtime_source(ScopeAccessRequest {
                relation_id: "team-config-from-evidence",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Revalidation,
                parent_token: None,
                identity_inputs: &foreign_identity,
                depth: 1,
                max_bytes: 4_096,
                max_rows: 0,
            })
            .unwrap()
            .observe_related_replace_refresh(&removed_state, &origin)
            .unwrap_err();
        assert_eq!(
            foreign_error.to_string(),
            "scoped observation source binding does not match the active attachment"
        );
        assert_eq!(
            foreign_pass
                .finish()
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "team-config-from-evidence")
                .unwrap()
                .trace[0]
                .outcome,
            AccessOutcome::Failed
        );

        let (missing, missing_report) = observe(b"missing-team", AccessPhase::Revalidation);
        assert!(missing.is_unavailable());
        assert_eq!(
            missing.identity().source().object_key,
            CoverageObjectKey::derive(
                "team-config-stream",
                &confined_relative_path_key(
                    PathBuf::from("teams/missing-team/config.json").as_path()
                )
                .unwrap(),
            )
            .unwrap()
        );
        assert!(!format!("{missing:?}").contains("missing-team"));
        let missing_relation = missing_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(missing_relation.bytes_read, 0);
        assert_eq!(
            missing_relation.trace[0].outcome,
            AccessOutcome::Unavailable
        );
        let missing_state = missing.refresh_state().unwrap();
        assert!(missing_state.checkpoint_for_test().is_none());
        let (still_missing, _) =
            refresh(b"missing-team", &missing_state, AccessPhase::Revalidation);
        assert!(still_missing.unwrap().is_unchanged_for_test());

        let (oversized, oversized_report) = observe(b"oversized-team", AccessPhase::Revalidation);
        assert_eq!(oversized.oversized().map(|value| value.0), Some(4_097));
        assert!(!format!("{oversized:?}").contains("oversized-team"));
        let oversized_relation = oversized_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(oversized_relation.bytes_read, 0);
        assert_eq!(
            oversized_relation.trace[0].outcome,
            AccessOutcome::Oversized
        );
        let oversized_state = oversized.refresh_state().unwrap();
        let (still_oversized, _) = refresh(
            b"oversized-team",
            &oversized_state,
            AccessPhase::Revalidation,
        );
        let still_oversized = still_oversized.unwrap();
        assert!(still_oversized.is_unchanged_for_test());
        let still_oversized_state = still_oversized.refresh_state().unwrap();
        std::fs::write(
            root.join("teams/oversized-team/config.json"),
            b"recovered-document",
        )
        .unwrap();
        let (recovered, _) = refresh(
            b"oversized-team",
            &still_oversized_state,
            AccessPhase::Revalidation,
        );
        let recovered = recovered.unwrap();
        assert_eq!(
            recovered.present_snapshot_for_test().unwrap().generation(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn related_object_origin_and_adapter_panic_fail_closed_without_private_output() {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(COMPOSED_RELATED_SCOPE_DOCUMENT);
        let registry = AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![
                        fixture_root_runtime_stream(),
                        fixture_related_runtime_stream(),
                    ])
                    .with_stateful_decode()
                    .with_dependency_free_bootstrap_panic(PathBuf::from(
                        "teams/private-panic-team/config.json",
                    )),
            )
            .register_native_support_probe("fixture", |_| {
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("panic-private-root");
        std::fs::create_dir_all(root.join("teams/private-panic-team")).unwrap();
        std::fs::write(
            root.join("teams/private-panic-team/config.json"),
            b"private-panic-payload",
        )
        .unwrap();
        let request = scoped_access_request(root);
        let source_instance_id = request.source_instance.id;
        let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
        let identity = [ScopeIdentityInput {
            name: "team-name",
            value: b"private-panic-team",
        }];
        let observe = |pass: &ScopedObservationAccessPass, origin: &RecordOrigin| {
            pass.reserve_observation_runtime_source(ScopeAccessRequest {
                relation_id: "team-config-from-evidence",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 4_096,
                max_rows: 0,
            })
            .unwrap()
            .observe_initial_related_replace(origin)
        };
        let origin = RecordOrigin {
            source_instance_id,
            stream_id: 81,
            object_id: 82,
            observed_at: 83,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        };

        let wrong_origin = RecordOrigin {
            source_instance_id: source_instance_id + 1,
            ..origin.clone()
        };
        let wrong_pass = host.begin_pass().unwrap();
        let wrong_error = observe(&wrong_pass, &wrong_origin).unwrap_err();
        assert_eq!(
            wrong_error.to_string(),
            "scoped observation source binding does not match the active attachment"
        );
        let wrong_report = wrong_pass.finish();
        let wrong_relation = wrong_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(wrong_relation.trace[0].outcome, AccessOutcome::Failed);

        let panic_pass = host.begin_pass().unwrap();
        let panic_error = observe(&panic_pass, &origin).unwrap_err();
        assert_eq!(
            panic_error.to_string(),
            "scoped observation related-object decode failed"
        );
        let rendered = format!("{panic_error:?}");
        assert!(rendered.contains("AdapterFatal"));
        for private in [
            "/Users/",
            "alice",
            "private-panic-team",
            "private-panic-payload",
            "config.json",
        ] {
            assert!(!rendered.contains(private));
        }
        let panic_report = panic_pass.finish();
        let panic_relation = panic_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap();
        assert_eq!(panic_relation.bytes_read, 4_096);
        assert_eq!(panic_relation.trace[0].outcome, AccessOutcome::Failed);
    }

    #[cfg(unix)]
    #[test]
    fn related_object_context_change_resets_generation_and_decoder_state() {
        let context_revision = Arc::new(AtomicUsize::new(1));
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(COMPOSED_RELATED_SCOPE_DOCUMENT);
        let registry = AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![
                        fixture_root_runtime_stream(),
                        fixture_related_runtime_stream(),
                    ])
                    .with_stateful_decode()
                    .with_dependency_free_context_revision(Arc::clone(&context_revision)),
            )
            .register_native_support_probe("fixture", |_| {
                Ok(NativeArtifactProbe {
                    family: "fixture".to_string(),
                    platform: "test".to_string(),
                    version: Some("1.0.0".to_string()),
                    markers: vec!["fixture.marker".to_string()],
                    contradictory_markers: false,
                })
            })
            .build_supported(catalog)
            .unwrap();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("context-private-root");
        std::fs::create_dir_all(root.join("teams/context-team")).unwrap();
        let payload = b"context-stable-document";
        std::fs::write(root.join("teams/context-team/config.json"), payload).unwrap();
        let request = scoped_access_request(root);
        let source_instance_id = request.source_instance.id;
        let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
        let identity = [ScopeIdentityInput {
            name: "team-name",
            value: b"context-team",
        }];
        let origin = RecordOrigin {
            source_instance_id,
            stream_id: 91,
            object_id: 92,
            observed_at: 93,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        };

        let initial_pass = host.begin_pass().unwrap();
        let initial = initial_pass
            .reserve_observation_runtime_source(ScopeAccessRequest {
                relation_id: "team-config-from-evidence",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 4_096,
                max_rows: 0,
            })
            .unwrap()
            .observe_initial_related_replace(&origin)
            .unwrap();
        let initial_snapshot = initial.present_snapshot().unwrap();
        assert_eq!(initial_snapshot.generation(), 1);
        assert_eq!(
            initial_snapshot.next_decoder_state_for_test(),
            Some(payload.as_slice())
        );
        let initial_state = initial.refresh_state().unwrap();
        initial_pass.finish();

        context_revision.store(2, Ordering::Release);
        let refresh_pass = host.begin_pass().unwrap();
        let refreshed = refresh_pass
            .reserve_observation_runtime_source(ScopeAccessRequest {
                relation_id: "team-config-from-evidence",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Revalidation,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 4_096,
                max_rows: 0,
            })
            .unwrap()
            .observe_related_replace_refresh(&initial_state, &origin)
            .unwrap();
        let refreshed_snapshot = refreshed.present_snapshot_for_test().unwrap();
        assert_eq!(refreshed_snapshot.generation(), 2);
        assert_eq!(refreshed_snapshot.revision(), Revision::digest(payload));
        assert_eq!(
            refreshed_snapshot.next_decoder_state_for_test(),
            Some(payload.as_slice())
        );
        let refreshed_state = refreshed.refresh_state().unwrap();
        assert_ne!(
            initial_state.object_context_for_test().payload(),
            refreshed_state.object_context_for_test().payload()
        );
        let relation = refresh_pass
            .finish()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "team-config-from-evidence")
            .unwrap()
            .clone();
        assert_eq!(relation.bytes_read, payload.len() as u64);
        assert_eq!(relation.trace[0].outcome, AccessOutcome::Available);
    }

    #[test]
    fn configured_runtime_prepares_dynamic_directory_authority_without_opening_it() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_dynamic_directory_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
        );
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("configured-dynamic-private-root");
        std::fs::create_dir_all(root.join("sessions/fixture-session/children")).unwrap();
        std::fs::write(root.join("sessions/session.jsonl"), b"fixture\n").unwrap();

        let attachment = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(vec![root], PathBuf::from("sessions/session.jsonl")),
        )
        .unwrap()
        .unwrap();
        let prepared = attachment.prepare_append_runtime(16, 16).unwrap();
        assert_eq!(prepared.objects().len(), 1);
        assert_eq!(prepared.bindings().len(), 1);
        assert_eq!(prepared.directory_bindings().len(), 1);
        let directory = &prepared.directory_bindings()[0];
        assert_eq!(directory.relation_id(), "descendant-objects");
        assert_eq!(
            directory.identity_input_names().collect::<Vec<_>>(),
            vec!["native-session-id"]
        );
        assert_eq!(directory.bounds().max_objects, 8);
        assert_eq!(prepared.required_coverage_objects(), 10);
        let rendered = format!("{prepared:?}");
        assert!(!rendered.contains("configured-dynamic-private-root"));
        assert!(!rendered.contains("fixture-session"));
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn configured_dynamic_directory_bootstrap_joins_members_before_ready() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_dynamic_directory_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
        );
        let temp = TempDir::new().unwrap();
        let root = temp
            .path()
            .join("configured-directory-bootstrap-private-root");
        let children = root.join("sessions/fixture-session/children");
        std::fs::create_dir_all(children.join("nested")).unwrap();
        std::fs::write(root.join("sessions/session.jsonl"), b"fixture\n").unwrap();
        std::fs::write(
            children.join("nested/child.jsonl"),
            b"{\"type\":\"child\"}\n",
        )
        .unwrap();
        let attachment = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(vec![root], PathBuf::from("sessions/session.jsonl")),
        )
        .unwrap()
        .unwrap();
        let prepared = attachment.prepare_append_runtime(16, 16).unwrap();
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let opened = prepared
            .open_with_watcher_factory(ConfiguredScopedObservationRuntimeOptions::default(), {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        let (mut runtime, handle, supervisor) = opened.into_parts();
        let mut supervisor_task = tokio::spawn(supervisor.run_until_stopped());
        let mut bootstrap = None;
        for _ in 0..8 {
            let yielded = tokio::select! {
                stopped = &mut supervisor_task => {
                    panic!("dynamic configured supervisor stopped before bootstrap: {:?}", stopped.unwrap());
                }
                yielded = runtime.next_event() => yielded.unwrap().unwrap(),
            };
            if let ScopedObservationEvent::ObserverBootstrapComplete { barrier } =
                &yielded.envelope.event
            {
                bootstrap = Some(Arc::clone(barrier));
            }
            runtime
                .acknowledge_applied(yielded.application_receipt())
                .unwrap();
            if bootstrap.is_some() {
                break;
            }
        }
        let bootstrap = bootstrap.expect("dynamic membership must enter the bootstrap barrier");
        assert_eq!(bootstrap.scope_coverage.relations().len(), 2);
        let decode = bootstrap
            .source_coverage
            .iter()
            .find(|coverage| coverage.coverage_domain == CoverageDomain::Decode)
            .unwrap();
        assert_eq!(
            decode.points.len() + decode.explicit_absence_or_deletion.len(),
            3,
            "root, directory membership, and selected child must be complete before Ready"
        );
        assert_eq!(registrations.lock().unwrap().len(), 1);
        std::fs::write(
            children.join("nested/child.jsonl"),
            b"{\"type\":\"child-replaced\"}\n",
        )
        .unwrap();

        let resync_task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.resync_applied_at(40).await }
        });
        let mut replacement = None;
        for _ in 0..32 {
            let yielded = tokio::select! {
                stopped = &mut supervisor_task => {
                    panic!("dynamic configured supervisor stopped during replacement: {:?}", stopped.unwrap());
                }
                yielded = runtime.next_event() => yielded.unwrap().unwrap(),
            };
            if let ScopedObservationEvent::ObserverResyncComplete { barrier } =
                &yielded.envelope.event
            {
                replacement = Some(Arc::clone(barrier));
            }
            runtime
                .acknowledge_applied(yielded.application_receipt())
                .unwrap();
            if replacement.is_some() {
                break;
            }
        }
        let replacement =
            replacement.expect("dynamic membership must enter the replacement barrier");
        assert_eq!(replacement.scope_epoch, 2);
        assert_ne!(
            replacement.coverage_snapshot_digest,
            bootstrap.snapshot_digest
        );
        assert_eq!(replacement.scope_coverage.relations().len(), 2);
        let decode = replacement
            .source_coverage
            .iter()
            .find(|coverage| coverage.coverage_domain == CoverageDomain::Decode)
            .unwrap();
        assert_eq!(
            decode.points.len() + decode.explicit_absence_or_deletion.len(),
            3,
            "replacement must retain root, directory membership, and selected child coverage"
        );
        assert!(matches!(
            resync_task.await.unwrap().unwrap(),
            ScopedObservationResyncResolution::Ready(resolved)
                if Arc::ptr_eq(&resolved, &replacement)
        ));

        std::fs::write(
            children.join("nested/child.jsonl"),
            b"{\"type\":\"child-polled\"}\n",
        )
        .unwrap();
        let poll_task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.poll().await }
        });
        let mut poll_required = None;
        let mut later_required = None;
        let mut later_replacement = None;
        let mut later_poll_ticket = None;
        for _ in 0..64 {
            let yielded = tokio::select! {
                stopped = &mut supervisor_task => {
                    panic!("dynamic configured supervisor stopped during polled replacement: {:?}", stopped.unwrap());
                }
                yielded = runtime.next_event() => yielded.unwrap().unwrap(),
            };
            match &yielded.envelope.event {
                ScopedObservationEvent::ObserverResyncRequired { control }
                    if control.invalid_scope_epoch == 2 =>
                {
                    poll_required = Some(Arc::clone(control));
                }
                ScopedObservationEvent::ObserverResyncRequired { control }
                    if control.invalid_scope_epoch == 3 =>
                {
                    later_required = Some(Arc::clone(control));
                }
                ScopedObservationEvent::ObserverResyncStarted { control }
                    if control.new_scope_epoch == 3 && later_poll_ticket.is_none() =>
                {
                    later_poll_ticket = Some(handle.host().request_poll().unwrap());
                }
                ScopedObservationEvent::ObserverResyncComplete { barrier }
                    if barrier.scope_epoch == 4 =>
                {
                    later_replacement = Some(Arc::clone(barrier));
                }
                _ => {}
            }
            runtime
                .acknowledge_applied(yielded.application_receipt())
                .unwrap();
            if later_replacement.is_some() {
                break;
            }
        }
        let poll_required = poll_required.expect("a dynamic poll must invalidate incrementality");
        assert_eq!(
            poll_required.reason,
            ScopedResyncReason::TransportContinuityLoss
        );
        let poll_watermark = match poll_task.await.unwrap().unwrap() {
            ScopedObservationPollResolution::Ready(watermark) => watermark,
            other => panic!("dynamic poll must resolve from replacement: {other:?}"),
        };
        assert_eq!(poll_watermark.scope_epoch, 3);
        assert_eq!(poll_watermark.scope_coverage.relations().len(), 2);
        let poll_decode = poll_watermark
            .source_coverage
            .iter()
            .find(|coverage| coverage.coverage_domain == CoverageDomain::Decode)
            .unwrap();
        assert_eq!(
            poll_decode.points.len() + poll_decode.explicit_absence_or_deletion.len(),
            3
        );
        let later_required = later_required.expect("the later poll must own a later replacement");
        assert_eq!(
            later_required.reason,
            ScopedResyncReason::TransportContinuityLoss
        );
        let later_replacement =
            later_replacement.expect("the later poll must complete a distinct replacement");
        assert_eq!(later_replacement.scope_epoch, 4);
        let later_watermark = match later_poll_ticket
            .expect("a request admitted after replacement start must remain independently owned")
            .wait_async()
            .await
        {
            ScopedObservationPollResolution::Ready(watermark) => watermark,
            other => panic!("later dynamic poll must resolve from its replacement: {other:?}"),
        };
        assert_eq!(later_watermark.scope_epoch, 4);

        let close = handle.request_close();
        let stopped = tokio::time::timeout(Duration::from_secs(2), supervisor_task)
            .await
            .unwrap()
            .unwrap();
        let ConfiguredScopedObservationSupervisorRunResult::Stopped(owners) = stopped else {
            panic!("configured dynamic owner must close structurally");
        };
        assert_eq!(owners.source().binding_count(), 1);
        assert_eq!(owners.source().directory_binding_count(), 1);
        assert!(close.wait_async().await.complete);
        assert!(runtime.next_event().await.unwrap().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(callback_slot.lock().unwrap().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn configured_append_supervisor_watches_before_scan_and_closes_structurally() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_attachment_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
            "1.0.0",
        );
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("configured-supervisor-private-root");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        let target = root.join("sessions/session.jsonl");
        let attachment = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(vec![root], PathBuf::from("sessions/session.jsonl")),
        )
        .unwrap()
        .unwrap();
        let prepared = attachment
            .prepare_append_runtime(16, 16)
            .unwrap()
            .with_forced_contract_replay_for_test();
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let opened = prepared
            .open_with_watcher_factory(ConfiguredScopedObservationRuntimeOptions::default(), {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        assert_eq!(registrations.lock().unwrap().len(), 1);
        assert!(callback_slot.lock().unwrap().is_some());

        // The transcript appears only after watcher installation. A cold
        // existing object does not fabricate a creation event, but the first
        // barrier must still bind it as present.
        std::fs::write(&target, b"fixture\n").unwrap();
        let (mut runtime, handle, supervisor) = opened.into_parts();
        let supervisor_task = tokio::spawn(supervisor.run_until_stopped());
        let mut saw_bootstrap = false;
        for _ in 0..8 {
            let yielded = tokio::time::timeout(Duration::from_secs(2), runtime.next_event())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            saw_bootstrap |= matches!(
                &yielded.envelope.event,
                ScopedObservationEvent::ObserverBootstrapComplete { barrier }
                    if barrier.root_present
            );
            runtime
                .acknowledge_applied(yielded.application_receipt())
                .unwrap();
            if saw_bootstrap {
                break;
            }
        }
        assert!(saw_bootstrap);
        assert!(matches!(
            handle.ready_applied().await.unwrap(),
            ScopedObservationReadyResolution::Ready(_)
        ));

        // A forced contract replay is consumed by bootstrap only. If the
        // configured supervisor leaked it into the live owner, this unchanged
        // poll would restart the object and emit a correction reset.
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), handle.poll())
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), runtime.next_event())
                .await
                .is_err()
        );

        let resync_task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.resync_applied().await }
        });
        let mut saw_resync_complete = false;
        for _ in 0..32 {
            let yielded = tokio::time::timeout(Duration::from_secs(2), runtime.next_event())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            saw_resync_complete |= matches!(
                &yielded.envelope.event,
                ScopedObservationEvent::ObserverResyncComplete { .. }
            );
            runtime
                .acknowledge_applied(yielded.application_receipt())
                .unwrap();
            if saw_resync_complete {
                break;
            }
        }
        assert!(saw_resync_complete);
        assert!(matches!(
            resync_task.await.unwrap().unwrap(),
            ScopedObservationResyncResolution::Ready(_)
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), handle.poll())
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));

        let close = handle.request_close();
        let stopped = tokio::time::timeout(Duration::from_secs(2), supervisor_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ConfiguredScopedObservationSupervisorRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert!(runtime.next_event().await.unwrap().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(callback_slot.lock().unwrap().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn configured_bootstrap_isolates_terminal_object_error_from_healthy_sibling() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry =
            configured_two_append_registry(Arc::clone(&probe_calls), Arc::clone(&discover_calls));
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("configured-isolation-private-root");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::create_dir_all(root.join("siblings")).unwrap();
        std::fs::write(root.join("sessions/session.jsonl"), b"healthy\n").unwrap();
        std::fs::write(root.join("siblings/sibling.jsonl"), b"stream-fatal\n").unwrap();
        let attachment = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_two_append_request(root),
        )
        .unwrap()
        .unwrap();
        let prepared = attachment.prepare_append_runtime(16, 16).unwrap();
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let opened = prepared
            .open_with_watcher_factory(ConfiguredScopedObservationRuntimeOptions::default(), {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        let (mut runtime, handle, supervisor) = opened.into_parts();
        let mut supervisor_task = tokio::spawn(supervisor.run_until_stopped());

        let mut bootstrap_barrier = None;
        for _ in 0..8 {
            let yielded = tokio::select! {
                stopped = &mut supervisor_task => {
                    panic!("configured supervisor stopped during isolated bootstrap: {:?}", stopped.unwrap());
                }
                yielded = runtime.next_event() => yielded.unwrap().unwrap(),
            };
            if let ScopedObservationEvent::ObserverBootstrapComplete { barrier } =
                &yielded.envelope.event
            {
                bootstrap_barrier = Some(Arc::clone(barrier));
            }
            runtime
                .acknowledge_applied(yielded.application_receipt())
                .unwrap();
            if bootstrap_barrier.is_some() {
                break;
            }
        }
        let barrier = bootstrap_barrier
            .expect("the isolated bootstrap should finish with one degraded relation");
        assert!(barrier.root_present);
        assert!(barrier
            .explicit_object_errors
            .iter()
            .any(|error| error.code == "decode_stream_fatal"));
        let poll_task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.poll().await }
        });
        let object_error = tokio::time::timeout(Duration::from_secs(2), runtime.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            &object_error.envelope.event,
            ScopedObservationEvent::SourceObjectError { error }
                if error.relation_id.as_ref() == "sibling-object"
                    && matches!(
                        error.retry,
                        ScopedSourceObjectRetryState::NotRetryable { .. }
                    )
        ));
        runtime
            .acknowledge_applied(object_error.application_receipt())
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), poll_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));

        let close = handle.request_close();
        let stopped = tokio::time::timeout(Duration::from_secs(2), supervisor_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ConfiguredScopedObservationSupervisorRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn configured_scoped_attachment_rejects_unbound_locator_without_path_leakage() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_attachment_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
            "1.0.0",
        );
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("private-configured-root");
        std::fs::create_dir_all(&root).unwrap();
        let error = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(vec![root], PathBuf::from("private/secret-source.txt")),
        )
        .unwrap_err();
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 1);
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            "invalid scoped access grant: configured scoped attachment does not match its promoted authority"
        );
        for private in [
            "private-configured-root",
            "secret-source",
            temp.path().to_str().unwrap(),
        ] {
            assert!(!rendered.contains(private));
        }
    }

    #[test]
    fn configured_append_runtime_redacts_decoder_bootstrap_failures() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_attachment_registry_with_decoder_bootstrap_failure(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
        );
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("private-configured-runtime-root");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        let attachment = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(vec![root], PathBuf::from("sessions/session.jsonl")),
        )
        .unwrap()
        .unwrap();

        let error = attachment.prepare_append_runtime(16, 16).unwrap_err();
        assert_eq!(
            error.to_string(),
            "scoped observation authorization failed: configured scoped runtime decoder binding failed"
        );
        for private in ["/Users/", "alice", "private", "session.jsonl"] {
            assert!(!error.to_string().contains(private));
        }
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn configured_scoped_attachment_redacts_probe_failures_before_discovery() {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(COMPOSED_ROOT_SCOPE_DOCUMENT);
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![fixture_root_runtime_stream()])
                    .with_configured_root_discovery(Arc::clone(&discover_calls)),
            )
            .register_native_support_probe("fixture", {
                let probe_calls = Arc::clone(&probe_calls);
                move |_| {
                    probe_calls.fetch_add(1, Ordering::AcqRel);
                    Err(AdapterError::new(
                        AdapterErrorClass::AdapterFatal,
                        "private_probe_failure",
                        "/Users/alice/private/session.jsonl",
                    ))
                }
            })
            .build_supported(catalog)
            .unwrap();
        let temp = TempDir::new().unwrap();
        let error = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(
                vec![temp.path().to_path_buf()],
                PathBuf::from("sessions/session.jsonl"),
            ),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "scoped observation authorization failed: trusted native support probe failed"
        );
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 0);
        assert!(!error.to_string().contains("/Users/"));
    }

    #[test]
    fn configured_scoped_attachment_contains_runtime_stream_panics() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_attachment_registry_with_stream_behavior(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
            "1.0.0",
            true,
        );
        let temp = TempDir::new().unwrap();
        let error = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(
                vec![temp.path().to_path_buf()],
                PathBuf::from("sessions/session.jsonl"),
            ),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid scoped access grant: configured scoped attachment does not match its promoted authority"
        );
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 1);
        assert!(!error.to_string().contains("/Users/"));
    }

    #[test]
    fn configured_scoped_attachment_requires_unambiguous_source_identity() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_attachment_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
            "1.0.0",
        );
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let error = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(
                vec![first.clone(), second.clone()],
                PathBuf::from("sessions/session.jsonl"),
            ),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid scoped access grant: configured scoped attachment source selection is ambiguous"
        );

        let canonical_source =
            CanonicalSourceInstanceKey::derive(1, &platform_path_key(&second)).unwrap();
        let session_key =
            CanonicalEntityKey::derive("fixture", &canonical_source, "session", b"fixture-session")
                .unwrap();
        let identity = ScopedConfiguredRootIdentity::new(
            b"fixture-session".as_slice(),
            BTreeMap::from([(
                "native-session-id".to_string(),
                Arc::<[u8]>::from(b"fixture-session".as_slice()),
            )]),
        )
        .unwrap()
        .with_expected_session(session_key, ExternalEntityRef::new(session_key));
        let selected = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request_with_identity(
                vec![first, second],
                PathBuf::from("sessions/session.jsonl"),
                identity,
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.host().root_identity().session_key, session_key);
        assert_eq!(probe_calls.load(Ordering::Acquire), 2);
        assert_eq!(discover_calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn unsupported_configured_scoped_attachment_never_discovers_or_falls_back() {
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let discover_calls = Arc::new(AtomicUsize::new(0));
        let registry = configured_attachment_registry(
            Arc::clone(&probe_calls),
            Arc::clone(&discover_calls),
            "9.9.9",
        );
        let temp = TempDir::new().unwrap();
        let result = prepare_configured_scoped_observation_attachment(
            &registry,
            configured_attachment_request(
                vec![temp.path().to_path_buf()],
                PathBuf::from("sessions/session.jsonl"),
            ),
        )
        .unwrap();
        assert!(result.is_none());
        assert_eq!(probe_calls.load(Ordering::Acquire), 1);
        assert_eq!(discover_calls.load(Ordering::Acquire), 0);
    }

    #[test]
    fn promoted_scoped_host_binds_the_exact_multi_family_projection() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("authorized-root");
        std::fs::create_dir_all(&root).unwrap();
        let request = multi_family_scoped_access_request(root);
        let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
        assert_eq!(
            host.contract_selection()
                .contract_versions
                .fact_family_versions,
            BTreeMap::from([
                ("runtime.actor-affiliation".to_owned(), 1),
                ("runtime.actor-run".to_owned(), 1),
                ("runtime.usage-v2".to_owned(), 1),
            ])
        );
        assert_eq!(host.capabilities().fact_families.len(), 3);
        assert!(host.capabilities().fact_families.iter().all(|family| {
            family.selected_version == Some(1)
                && host
                    .contract_selection()
                    .contract_versions
                    .fact_family_versions
                    .contains_key(&family.fact_family)
        }));
        let projection = host
            .open_projection_sink(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 8,
            })
            .unwrap();

        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
            vec![
                CoverageDomain::FactFamily {
                    family: "runtime.actor-affiliation".to_owned(),
                    version: 1,
                },
                CoverageDomain::FactFamily {
                    family: "runtime.actor-run".to_owned(),
                    version: 1,
                },
                CoverageDomain::FactFamily {
                    family: "runtime.usage-v2".to_owned(),
                    version: 1,
                },
            ],
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"multi-family-session",
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
            panic!("missing multi-family root should decode as empty");
        };
        let mut admission = admission_lane(1, 0, 1);
        if let Err(failure) = admission.admit(&mut object, &observation, decoded) {
            panic!("multi-family admission failed: {}", failure.error);
        }
        drop(pass);
        object.complete_bootstrap().unwrap();
        let mut delivery = ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
        let usage_only = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 8,
        })
        .unwrap();
        assert_eq!(
            host.capture_watermark_core(&admission, &usage_only, &delivery),
            Err(ScopedCoverageAssemblyError::InvalidContract)
        );
        let barrier = host
            .offer_bootstrap_complete(
                std::slice::from_ref(&object),
                &admission,
                &projection,
                &mut delivery,
                50,
            )
            .unwrap();
        assert_eq!(barrier.source_coverage.len(), 4);
        assert_eq!(
            barrier
                .family_manifest
                .iter()
                .map(|family| family.fact_family.as_str())
                .collect::<Vec<_>>(),
            vec![
                "runtime.actor-affiliation",
                "runtime.actor-run",
                "runtime.usage-v2",
            ]
        );
        assert!(barrier
            .family_manifest
            .iter()
            .all(|family| family.entity_or_event_count == 0));
    }

    #[test]
    fn promoted_scoped_host_requires_directory_membership_before_bootstrap_completion() {
        let registry = supported_fixture_registry_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("authorized-root");
        std::fs::create_dir_all(&root).unwrap();

        let host = ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root))
            .expect("ChildDirectoryByNativeId may attach when membership will be recorded");
        let admission = admission_lane_for_objects(8);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        let _ticket = host.request_poll().unwrap();
        let lease = host.begin_poll().unwrap().unwrap();
        assert!(matches!(
            host.complete_bootstrap_poll(lease, &admission, &projection, &drain),
            Err(ScopedObservationPollError::IncompleteScopePass)
        ));
    }

    #[test]
    fn rfc012_d2_host_composes_child_directory_membership_before_bootstrap_completion() {
        let registry = supported_fixture_registry_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("composed-root");
        let children = root.join("sessions/fixture-session/children");
        std::fs::create_dir_all(children.join("nested")).unwrap();
        std::fs::write(
            children.join("nested/child.jsonl"),
            b"{\"type\":\"child\"}\n",
        )
        .unwrap();

        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let mut admission = admission_lane_for_objects(8);
        let mut projection =
            ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 1,
            })
            .unwrap();
        let mut drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 4,
                max_retained_native_bytes: 0,
                max_source_control_items: 4,
            })
            .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"fixture-session",
        }];
        let origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };

        let ticket = host.request_poll().unwrap();
        let lease = host.begin_poll().unwrap().unwrap();
        let mut root_object = scoped_append_object_for_native_object(b"session.jsonl");
        reconcile_named_relation(
            &host,
            lease.access_pass(),
            &mut root_object,
            &mut admission,
            named_relation_request("root-object", &identity, &origin, AccessPhase::Initial),
        );
        let foreign_host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        let foreign_pass = foreign_host.begin_pass().unwrap();
        assert!(matches!(
            host.scan_directory_relation_membership(
                &foreign_pass,
                "descendant-objects",
                &identity,
                AccessPhase::Initial,
                None,
            ),
            Err(ScopedObservationAccessError::InvalidGrant(_))
        ));
        assert_eq!(
            foreign_pass
                .report()
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "descendant-objects")
                .unwrap()
                .attempts,
            0
        );
        drop(foreign_pass);
        drop(foreign_host);
        let mut listing = match host
            .scan_directory_relation_membership(
                lease.access_pass(),
                "descendant-objects",
                &identity,
                AccessPhase::Initial,
                None,
            )
            .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
            other => panic!("expected a directory snapshot, got {other:?}"),
        };
        assert!(listing.selected_entry_count() >= 1);
        let mut contents = Vec::new();
        while let Some(read) = listing.read_next_member().unwrap() {
            match read {
                ScopedObservationDirectoryMemberRead::Stable(content) => contents.push(content),
                other => panic!("expected a stable directory member, got {other:?}"),
            }
        }
        assert!(listing.member_reads_complete());
        let mut lifecycles = Vec::with_capacity(contents.len());
        for content in contents {
            let mut member_origin = origin.clone();
            member_origin.source_instance_id = content.source_instance_for_test().id;
            let input = content.bootstrap_for_test().unwrap();
            lifecycles.push(
                listing
                    .observe_bootstrapped_member(input, &member_origin, None)
                    .unwrap(),
            );
        }
        let verification = match host
            .scan_directory_relation_membership(
                lease.access_pass(),
                "descendant-objects",
                &identity,
                AccessPhase::Initial,
                Some(&listing),
            )
            .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected a verification snapshot, got {other:?}"),
        };
        listing.confirm_membership_unchanged(verification).unwrap();
        let membership = ScopedRelationMembershipObservation::from_directory_listing(listing)
            .expect("membership observation");
        admission
            .record_relation_membership(
                lease.access_pass().pass_id(),
                ScopedAppendDeliveryPhase::Bootstrap,
                membership,
            )
            .unwrap();
        for lifecycle in lifecycles {
            admission
                .admit_directory_member(
                    lease.access_pass().pass_id(),
                    ScopedAppendDeliveryPhase::Bootstrap,
                    lifecycle,
                )
                .unwrap();
        }
        while !admission.is_empty() {
            assert!(host
                .offer_consumer_next(&mut admission, &mut projection, &mut drain)
                .unwrap()
                .is_some());
        }
        let report = lease.access_pass().report();
        let directory = report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap();
        assert_eq!(directory.attempts, 7);
        assert_eq!(directory.completed, 7);
        assert_eq!(directory.abandoned, 0);
        let watermark = host
            .complete_bootstrap_poll(lease, &admission, &projection, &drain)
            .unwrap();
        assert!(matches!(
            ticket.wait(),
            ScopedObservationPollResolution::Ready(ready) if Arc::ptr_eq(&ready, &watermark)
        ));
        let decode = watermark
            .source_coverage
            .iter()
            .find(|coverage| coverage.coverage_domain == CoverageDomain::Decode)
            .unwrap();
        assert_eq!(
            decode.points.len() + decode.explicit_absence_or_deletion.len(),
            3,
            "root, membership source, and selected child must all remain covered"
        );
        assert_eq!(watermark.scope_coverage.relations().len(), 2);
        let rendered = format!("{watermark:?}");
        assert!(!rendered.contains("fixture-session"));
        assert!(!rendered.contains("child.jsonl"));
        assert!(!rendered.contains("composed-root"));
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

    #[test]
    fn attachment_retains_one_exact_private_source_instance() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("authorized-root");
        std::fs::create_dir_all(&root).unwrap();

        let rendered = format!(
            "{:?}",
            scoped_access_request(PathBuf::from("/Users/alice/private/source.jsonl"))
        );
        for private in ["/Users/", "alice", "private", "source.jsonl"] {
            assert!(!rendered.contains(private));
        }

        let valid =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        assert_eq!(
            valid.root_identity().source_instance_key,
            CanonicalSourceInstanceKey::derive(1, b"fixture-source-instance").unwrap()
        );
        drop(valid);

        let assert_invalid = |request: ScopedObservationAccessRequest| {
            let error = match ScopedObservationAccessHost::authorize(&registry, request) {
                Ok(_) => panic!("a substituted source instance must fail attachment"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                ScopedObservationAccessError::InvalidGrant(ref message)
                    if message == "the attachment source instance does not match its approved identity and roots"
            ));
            let rendered = error.to_string();
            for private in ["/Users/", "alice", "private", "source.jsonl"] {
                assert!(!rendered.contains(private));
            }
        };

        let mut zero_id = scoped_access_request(root.clone());
        zero_id.source_instance.id = 0;
        assert_invalid(zero_id);

        let mut wrong_identity_contract = scoped_access_request(root.clone());
        wrong_identity_contract
            .source_instance
            .spec
            .identity_contract_version = 2;
        assert_invalid(wrong_identity_contract);

        let mut substituted_identity = scoped_access_request(root.clone());
        substituted_identity.source_instance.spec.stable_key =
            SourceInstanceKey::new(b"/Users/alice/private/source.jsonl".to_vec()).unwrap();
        assert_invalid(substituted_identity);

        let mut substituted_root = scoped_access_request(root.clone());
        substituted_root.source_instance.spec.roots[0].path =
            PathBuf::from("/Users/alice/private/source.jsonl");
        assert_invalid(substituted_root);

        let mut duplicate_root = scoped_access_request(root);
        duplicate_root
            .source_instance
            .spec
            .roots
            .push(duplicate_root.source_instance.spec.roots[0].clone());
        assert_invalid(duplicate_root);
    }

    #[test]
    fn closed_pass_rejects_runtime_source_binding_before_reserving_access() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("authorized-root");
        std::fs::create_dir_all(&root).unwrap();
        let host =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root)).unwrap();
        let pass = host.begin_pass().unwrap();
        let _barrier = host.close();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"secret-session-id",
        }];
        let error = pass
            .reserve_observation_runtime_source(ScopeAccessRequest {
                relation_id: "root-object",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 128,
                max_rows: 0,
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "scoped observation host closed before source binding completed"
        );
        let root_report = pass
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "root-object")
            .unwrap()
            .clone();
        assert_eq!(root_report.attempts, 0);
        assert_eq!(root_report.completed, 0);
    }

    #[test]
    fn typed_scope_authorization_alone_mints_bound_observation_source_reservations() {
        let registry = supported_fixture_registry_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        let temp = TempDir::new().unwrap();
        let request = scoped_access_request(temp.path().join("authorized-root"));
        let (_, authorization) = registry
            .authorize_typed_access(
                &AdapterId::new("fixture").unwrap(),
                &request.artifact_probe,
                SupportOperation::ScopedTypedObservation,
                &request.observation_contract_request.contract_versions,
                &request.observation_contract_offer.contract_versions,
            )
            .unwrap();
        let program = authorization
            .select_scope_program("observe-session")
            .unwrap();
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program).unwrap();
        let report = plan.report();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"secret-session-id",
        }];

        let reservation = plan
            .reserve_observation_source(ScopeAccessRequest {
                relation_id: "descendant-objects",
                operation: AccessOperation::ObjectListing,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 1_024,
                max_rows: 0,
            })
            .unwrap();
        assert_eq!(reservation.relation_id(), "descendant-objects");
        assert_eq!(
            reservation.primitive(),
            ScopeRelationPrimitive::ChildDirectoryByNativeId
        );
        assert_eq!(reservation.access_root(), "root");
        assert_eq!(reservation.stream_id(), "descendant-stream");
        assert_eq!(
            reservation.driver(),
            AuthorizedObservationSourceDriver::ReplaceDocument {
                max_object_bytes: 1_024,
            }
        );
        assert_eq!(reservation.source_pattern(), "sessions/*/children/**");
        assert_eq!(reservation.relative_selector(), Some("**"));
        assert_eq!(
            reservation.locator(),
            PathBuf::from("sessions/secret-session-id/children").as_path()
        );
        assert_eq!(
            reservation.support_release_digest(),
            report.support_release_digest()
        );
        assert_eq!(
            reservation.source_declaration_digest(),
            plan.source_declaration_digest()
        );
        assert_eq!(
            reservation.scope_program_digest(),
            report.scope_program_digest()
        );
        assert_eq!(
            reservation.object_token(),
            AccessObjectToken::derive(
                "descendant-objects",
                &[
                    b"native-session-id".as_slice(),
                    b"secret-session-id".as_slice(),
                ],
            )
            .unwrap()
        );
        let rendered_debug = format!("{reservation:?}");
        assert!(!rendered_debug.contains("secret-session-id"));
        assert!(!rendered_debug.contains("descendant-stream"));
        assert!(!rendered_debug.contains("sessions/"));
        reservation.complete(512, AccessOutcome::Available).unwrap();

        let path_shaped_identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"different/session-id",
        }];
        let error = plan
            .reserve_observation_source(ScopeAccessRequest {
                relation_id: "descendant-objects",
                operation: AccessOperation::ObjectListing,
                phase: AccessPhase::Revalidation,
                parent_token: None,
                identity_inputs: &path_shaped_identity,
                depth: 1,
                max_bytes: 1_024,
                max_rows: 0,
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid access-budget configuration: scope locator template or bound identity input is invalid"
        );
        let rendered = error.to_string();
        assert!(!rendered.contains("secret-session-id"));
        assert!(!rendered.contains("different/session-id"));

        let error = plan
            .reserve_observation_source(ScopeAccessRequest {
                relation_id: "/Users/alice/private/session.jsonl",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 128,
                max_rows: 0,
            })
            .unwrap_err();
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            "invalid access-budget configuration: authorized observation source reservation requires one supported bound relation"
        );
        assert!(!rendered.contains("/Users/"));
        assert!(!rendered.contains("alice"));

        let error = plan
            .reserve_observation_source(ScopeAccessRequest {
                relation_id: "root-object",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 128,
                max_rows: 0,
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid access-budget configuration: authorized observation source reservation requires one supported bound relation"
        );
        assert!(!error.to_string().contains("root-object"));

        let report = plan.report();
        assert!(report.verify_digest());
        let descendant = report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(descendant.attempts, 2);
        assert_eq!(descendant.completed, 2);
        assert_eq!(descendant.abandoned, 0);
        assert_eq!(descendant.trace[0].outcome, AccessOutcome::Available);
        assert_eq!(descendant.trace[1].outcome, AccessOutcome::Failed);
        let root = report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "root-object")
            .unwrap();
        assert_eq!(root.attempts, 0);
        assert_eq!(root.completed, 0);
        assert_eq!(root.abandoned, 0);
        assert!(root.trace.is_empty());
    }

    #[test]
    fn observation_source_reservation_binds_one_exact_adapter_runtime_stream() {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        let registry = AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding.clone(), scope_programs.clone())
                    .with_streams(vec![fixture_descendant_runtime_stream()]),
            )
            .build_supported(catalog)
            .unwrap();
        let request = scoped_access_request(PathBuf::from("/private/fixture-root"));
        let instance = SourceInstance {
            id: 7,
            spec: SourceInstanceSpec {
                identity_contract_version: 1,
                stable_key: SourceInstanceKey::new(
                    b"/Users/alice/private/source-instance".to_vec(),
                )
                .unwrap(),
                display_name: "private fixture source".to_string(),
                roots: vec![SourceRoot {
                    name: "root".to_string(),
                    path: PathBuf::from("/Users/alice/private/root"),
                }],
                discovery_reason: "fixture".to_string(),
            },
        };
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"secret-session-id",
        }];
        let make_plan = || {
            let (_, authorization) = registry
                .authorize_typed_access(
                    &AdapterId::new("fixture").unwrap(),
                    &request.artifact_probe,
                    SupportOperation::ScopedTypedObservation,
                    &request.observation_contract_request.contract_versions,
                    &request.observation_contract_offer.contract_versions,
                )
                .unwrap();
            AuthorizedScopeAccessPlan::from_authorized_program(
                authorization
                    .select_scope_program("observe-session")
                    .unwrap(),
            )
            .unwrap()
        };
        let reserve = |plan: &AuthorizedScopeAccessPlan| {
            plan.reserve_observation_source(ScopeAccessRequest {
                relation_id: "descendant-objects",
                operation: AccessOperation::ObjectListing,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 1_024,
                max_rows: 0,
            })
            .unwrap()
        };

        let plan = make_plan();
        let adapter = registry.get(&AdapterId::new("fixture").unwrap()).unwrap();
        let bound = reserve(&plan)
            .bind_runtime_stream(adapter.as_ref(), &instance)
            .unwrap();
        assert_eq!(bound.relation_id(), "descendant-objects");
        assert_eq!(bound.access_root(), "root");
        assert_eq!(
            bound.locator(),
            PathBuf::from("sessions/secret-session-id/children").as_path()
        );
        assert_eq!(bound.source_instance_id(), 7);
        assert_eq!(bound.source_instance_identity_contract_version(), 1);
        assert_eq!(
            bound.source_instance_key().as_bytes(),
            b"/Users/alice/private/source-instance"
        );
        assert_eq!(bound.stream().id.as_str(), "descendant-stream");
        assert_eq!(bound.stream().decoder.as_str(), "fixture-descendant");
        assert_eq!(
            bound.object_token(),
            AccessObjectToken::derive(
                "descendant-objects",
                &[
                    b"native-session-id".as_slice(),
                    b"secret-session-id".as_slice(),
                ],
            )
            .unwrap()
        );
        assert_eq!(
            bound.support_release_digest(),
            plan.report().support_release_digest()
        );
        assert_eq!(
            bound.source_declaration_digest(),
            plan.source_declaration_digest()
        );
        assert_eq!(
            bound.scope_program_digest(),
            plan.report().scope_program_digest()
        );
        let rendered = format!("{bound:?}");
        for private in [
            "secret-session-id",
            "fixture-descendant",
            "descendant-stream",
            "/Users/",
            "alice",
            "private",
        ] {
            assert!(!rendered.contains(private));
        }
        let approved_root = ScopedAccessRootGrant {
            access_root: "root".to_string(),
            root: PathBuf::from("/Users/alice/private/root"),
        };
        let expected_source_instance_key = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        )
        .unwrap();
        let rooted = bind_observation_runtime_source_for_test(
            bound,
            Arc::clone(adapter),
            Arc::new(instance.clone()),
            &approved_root,
            &expected_source_instance_key,
        )
        .unwrap();
        assert_eq!(rooted.relation_id(), "descendant-objects");
        assert_eq!(rooted.access_root(), "root");
        assert_eq!(rooted.root(), approved_root.root.as_path());
        assert_eq!(
            rooted.locator(),
            PathBuf::from("sessions/secret-session-id/children").as_path()
        );
        assert_eq!(rooted.relative_selector(), Some("**"));
        assert_eq!(rooted.source_instance_id(), 7);
        assert_eq!(
            rooted.source_instance_key().as_bytes(),
            b"/Users/alice/private/source-instance"
        );
        assert_eq!(rooted.stream().decoder.as_str(), "fixture-descendant");
        assert_eq!(
            rooted.object_token(),
            AccessObjectToken::derive(
                "descendant-objects",
                &[
                    b"native-session-id".as_slice(),
                    b"secret-session-id".as_slice(),
                ],
            )
            .unwrap()
        );
        let rendered = format!("{rooted:?}");
        for private in [
            "secret-session-id",
            "fixture-descendant",
            "descendant-stream",
            "/Users/",
            "alice",
            "private",
        ] {
            assert!(!rendered.contains(private));
        }
        let mut directory = prepare_observation_directory_membership_for_test(&rooted).unwrap();
        assert_eq!(directory.config().max_entries, 7);
        assert_eq!(directory.config().max_entries_per_directory, 7);
        assert_eq!(directory.config().max_depth, 1);
        assert_eq!(
            directory.select(
                PathBuf::from("nested").as_path(),
                DirectoryEntryKind::Directory
            ),
            DirectorySelection::Recurse
        );
        assert_eq!(
            directory.select(
                PathBuf::from("nested/agent-child.jsonl").as_path(),
                DirectoryEntryKind::File,
            ),
            DirectorySelection::Include
        );
        assert_eq!(
            directory.select(
                PathBuf::from("../private/agent-child.jsonl").as_path(),
                DirectoryEntryKind::File,
            ),
            DirectorySelection::Ignore
        );
        assert_eq!(
            directory.select(
                PathBuf::from("../private").as_path(),
                DirectoryEntryKind::Directory,
            ),
            DirectorySelection::Ignore
        );
        assert_eq!(directory.source().adapter_id.as_str(), "fixture");
        assert_eq!(
            directory.source().source_instance_key,
            expected_source_instance_key
        );
        let rendered = format!("{directory:?}");
        for private in [
            "secret-session-id",
            "fixture-descendant",
            "descendant-stream",
            "/Users/",
            "alice",
            "private",
        ] {
            assert!(!rendered.contains(private));
        }
        let nested = directory
            .reserve_entry(&rooted, PathBuf::from("nested").as_path())
            .unwrap()
            .complete(DirectoryEntryKind::Directory)
            .unwrap();
        assert_eq!(nested.kind(), DirectoryEntryKind::Directory);
        assert_eq!(nested.depth(), 1);
        let child_reservation = directory
            .reserve_entry(&rooted, PathBuf::from("nested/agent-child.jsonl").as_path())
            .unwrap();
        assert_eq!(
            child_reservation.selection(DirectoryEntryKind::File),
            DirectorySelection::Include
        );
        assert_ne!(child_reservation.object_token(), nested.object_token());
        let rendered = format!("{child_reservation:?}");
        for private in ["nested", "agent-child", "jsonl", "/Users/", "alice"] {
            assert!(!rendered.contains(private));
        }
        let child = child_reservation
            .complete(DirectoryEntryKind::File)
            .unwrap();
        assert_eq!(child.kind(), DirectoryEntryKind::File);
        assert_eq!(child.depth(), 2);
        assert_ne!(child.object_token(), nested.object_token());
        rooted.complete(512, AccessOutcome::Available).unwrap();
        let descendant_report = plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(descendant_report.attempts, 3);
        assert_eq!(descendant_report.completed, 3);
        assert_eq!(descendant_report.objects_accessed, 3);
        assert_eq!(descendant_report.trace.len(), 3);
        assert_eq!(descendant_report.trace[0].parent_token, None);
        assert_eq!(
            descendant_report.trace[1].parent_token,
            Some(descendant_report.trace[0].object_token)
        );
        assert_eq!(
            descendant_report.trace[2].parent_token,
            Some(nested.object_token())
        );
        assert_eq!(descendant_report.trace[1].depth, 1);
        assert_eq!(descendant_report.trace[2].depth, 2);

        let assert_runtime_drift = |streams: Vec<StreamSpec>| {
            let plan = make_plan();
            let adapter = EmptyAdapter::new("fixture")
                .with_support(binding.clone(), scope_programs.clone())
                .with_streams(streams);
            let error = reserve(&plan)
                .bind_runtime_stream(&adapter, &instance)
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "invalid access-budget configuration: authorized observation source does not match the selected adapter runtime stream"
            );
            for private in ["/Users/", "alice", "private", "secret-session-id"] {
                assert!(!error.to_string().contains(private));
            }
            let report = plan.report();
            let relation = report
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "descendant-objects")
                .unwrap();
            assert_eq!(relation.attempts, 1);
            assert_eq!(relation.completed, 1);
            assert_eq!(relation.trace[0].outcome, AccessOutcome::Failed);
        };

        assert_runtime_drift(Vec::new());
        assert_runtime_drift(vec![
            fixture_descendant_runtime_stream(),
            fixture_descendant_runtime_stream(),
        ]);

        let mut drift = fixture_descendant_runtime_stream();
        drift.decoder = DecoderId::new("/Users/alice/private/decoder").unwrap();
        assert_runtime_drift(vec![drift]);

        let mut drift = fixture_descendant_runtime_stream();
        drift.selector.include.push("private/**".to_string());
        assert_runtime_drift(vec![drift]);

        let mut drift = fixture_descendant_runtime_stream();
        drift
            .selector
            .exclude
            .push("sessions/private/**".to_string());
        assert_runtime_drift(vec![drift]);

        let mut drift = fixture_descendant_runtime_stream();
        drift.driver = DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
            max_document_bytes: 1_025,
        });
        assert_runtime_drift(vec![drift]);

        let mut drift = fixture_descendant_runtime_stream();
        drift.authority = StreamAuthority::Supplemental;
        assert_runtime_drift(vec![drift]);

        let mut drift = fixture_descendant_runtime_stream();
        drift.deletion = DeletionPolicy::PreserveHistory;
        assert_runtime_drift(vec![drift]);

        let wrong_binding = AdapterSupportBinding::new(
            "private-release",
            binding.adapter_package_version(),
            binding.decoder_contract_version(),
            &binding.ads_digest().to_string(),
            &binding.source_declaration_digest().to_string(),
            &binding.scope_program_digest().to_string(),
        )
        .unwrap();
        let wrong_adapter = EmptyAdapter::new("fixture")
            .with_support(wrong_binding, scope_programs.clone())
            .with_streams(vec![fixture_descendant_runtime_stream()]);
        let plan = make_plan();
        let error = reserve(&plan)
            .bind_runtime_stream(&wrong_adapter, &instance)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid access-budget configuration: authorized observation source does not match the selected adapter runtime stream"
        );
        assert!(!error.to_string().contains("private-release"));

        let plan = make_plan();
        let wrong_instance = SourceInstance {
            id: 8,
            spec: SourceInstanceSpec {
                identity_contract_version: 1,
                stable_key: SourceInstanceKey::new(b"wrong-source".to_vec()).unwrap(),
                display_name: "wrong".to_string(),
                roots: vec![SourceRoot {
                    name: "other-root".to_string(),
                    path: PathBuf::from("/Users/alice/private/other"),
                }],
                discovery_reason: "fixture".to_string(),
            },
        };
        let error = reserve(&plan)
            .bind_runtime_stream(adapter.as_ref(), &wrong_instance)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid access-budget configuration: authorized observation source does not match the selected adapter runtime stream"
        );
        assert!(!error.to_string().contains("/Users/"));

        let assert_root_drift =
            |supplied_instance: &SourceInstance, supplied_root: ScopedAccessRootGrant| {
                let plan = make_plan();
                let runtime = reserve(&plan)
                    .bind_runtime_stream(adapter.as_ref(), &instance)
                    .unwrap();
                let error = bind_observation_runtime_source_for_test(
                    runtime,
                    Arc::clone(adapter),
                    Arc::new(supplied_instance.clone()),
                    &supplied_root,
                    &expected_source_instance_key,
                )
                .unwrap_err();
                assert_eq!(
                    error.to_string(),
                    "scoped observation source binding does not match the active attachment"
                );
                for private in ["/Users/", "alice", "private", "secret-session-id"] {
                    assert!(!error.to_string().contains(private));
                }
                let report = plan.report();
                let relation = report
                    .relations()
                    .iter()
                    .find(|relation| relation.relation_id == "descendant-objects")
                    .unwrap();
                assert_eq!(relation.attempts, 1);
                assert_eq!(relation.completed, 1);
                assert_eq!(relation.trace[0].outcome, AccessOutcome::Failed);
            };
        assert_root_drift(
            &instance,
            ScopedAccessRootGrant {
                access_root: "root".to_string(),
                root: PathBuf::from("/Users/alice/private/substituted-root"),
            },
        );
        assert_root_drift(
            &instance,
            ScopedAccessRootGrant {
                access_root: "/Users/alice/private/root-label".to_string(),
                root: approved_root.root.clone(),
            },
        );
        let substituted_instance = SourceInstance {
            id: instance.id,
            spec: SourceInstanceSpec {
                identity_contract_version: instance.spec.identity_contract_version,
                stable_key: SourceInstanceKey::new(b"substituted-source".to_vec()).unwrap(),
                display_name: "substituted".to_string(),
                roots: instance.spec.roots.clone(),
                discovery_reason: "fixture".to_string(),
            },
        };
        assert_root_drift(&substituted_instance, approved_root);

        let plan = make_plan();
        let runtime = reserve(&plan)
            .bind_runtime_stream(adapter.as_ref(), &instance)
            .unwrap();
        let foreign_source_instance_key =
            CanonicalSourceInstanceKey::derive(1, b"foreign-source-instance").unwrap();
        let error = bind_observation_runtime_source_for_test(
            runtime,
            Arc::clone(adapter),
            Arc::new(instance.clone()),
            &ScopedAccessRootGrant {
                access_root: "root".to_string(),
                root: PathBuf::from("/Users/alice/private/root"),
            },
            &foreign_source_instance_key,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "scoped observation source binding does not match the active attachment"
        );
    }

    #[test]
    fn directory_membership_entry_accounting_is_bounded_and_fail_closed() {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        let registry = AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![fixture_descendant_runtime_stream()]),
            )
            .build_supported(catalog)
            .unwrap();
        let request = scoped_access_request(PathBuf::from("/private/fixture-root"));
        let instance = SourceInstance {
            id: 7,
            spec: SourceInstanceSpec {
                identity_contract_version: 1,
                stable_key: SourceInstanceKey::new(
                    b"/Users/alice/private/source-instance".to_vec(),
                )
                .unwrap(),
                display_name: "private fixture source".to_string(),
                roots: vec![SourceRoot {
                    name: "root".to_string(),
                    path: PathBuf::from("/Users/alice/private/root"),
                }],
                discovery_reason: "fixture".to_string(),
            },
        };
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"secret-session-id",
        }];
        let approved_root = ScopedAccessRootGrant {
            access_root: "root".to_string(),
            root: PathBuf::from("/Users/alice/private/root"),
        };
        let expected_source_instance_key = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        )
        .unwrap();
        let adapter = registry.get(&AdapterId::new("fixture").unwrap()).unwrap();
        let make_bound = || {
            let (_, authorization) = registry
                .authorize_typed_access(
                    &AdapterId::new("fixture").unwrap(),
                    &request.artifact_probe,
                    SupportOperation::ScopedTypedObservation,
                    &request.observation_contract_request.contract_versions,
                    &request.observation_contract_offer.contract_versions,
                )
                .unwrap();
            let plan = AuthorizedScopeAccessPlan::from_authorized_program(
                authorization
                    .select_scope_program("observe-session")
                    .unwrap(),
            )
            .unwrap();
            let runtime = plan
                .reserve_observation_source(ScopeAccessRequest {
                    relation_id: "descendant-objects",
                    operation: AccessOperation::ObjectListing,
                    phase: AccessPhase::Initial,
                    parent_token: None,
                    identity_inputs: &identity,
                    depth: 1,
                    max_bytes: 1_024,
                    max_rows: 0,
                })
                .unwrap()
                .bind_runtime_stream(adapter.as_ref(), &instance)
                .unwrap();
            let rooted = bind_observation_runtime_source_for_test(
                runtime,
                Arc::clone(adapter),
                Arc::new(instance.clone()),
                &approved_root,
                &expected_source_instance_key,
            )
            .unwrap();
            (plan, rooted)
        };

        let (plan, rooted) = make_bound();
        let mut directory = prepare_observation_directory_membership_for_test(&rooted).unwrap();
        let orphan_error = directory
            .reserve_entry(&rooted, PathBuf::from("missing/private.jsonl").as_path())
            .unwrap_err();
        assert_eq!(
            orphan_error.to_string(),
            "scoped observation source binding does not match the active attachment"
        );
        assert!(directory.is_failed());
        assert_eq!(directory.accounted_entries(), 0);
        for private in ["missing", "private.jsonl", "/Users/", "alice"] {
            assert!(!orphan_error.to_string().contains(private));
        }
        rooted.fail_conservative();
        let relation = plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(relation.attempts, 1);
        assert_eq!(relation.objects_accessed, 1);
        assert_eq!(relation.trace[0].outcome, AccessOutcome::Failed);

        let (plan, rooted) = make_bound();
        let (foreign_plan, foreign_rooted) = make_bound();
        let mut directory = prepare_observation_directory_membership_for_test(&rooted).unwrap();
        let substitution_error = directory
            .reserve_entry(
                &foreign_rooted,
                PathBuf::from("private-substitution.jsonl").as_path(),
            )
            .unwrap_err();
        assert_eq!(
            substitution_error.to_string(),
            "invalid access-budget configuration: authorized directory entry reservation requires one confined enumerated child"
        );
        assert!(directory.is_failed());
        assert_eq!(directory.accounted_entries(), 0);
        for private in ["private-substitution", "jsonl", "/Users/", "alice"] {
            assert!(!substitution_error.to_string().contains(private));
        }
        rooted.fail_conservative();
        foreign_rooted.fail_conservative();
        for report in [plan.report(), foreign_plan.report()] {
            let relation = report
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "descendant-objects")
                .unwrap();
            assert_eq!(relation.attempts, 1);
            assert_eq!(relation.objects_accessed, 1);
        }

        let (plan, rooted) = make_bound();
        let mut directory = prepare_observation_directory_membership_for_test(&rooted).unwrap();
        let dropped = directory
            .reserve_entry(&rooted, PathBuf::from("private-abandoned.jsonl").as_path())
            .unwrap();
        drop(dropped);
        assert!(directory.is_failed());
        assert_eq!(directory.accounted_entries(), 0);
        rooted.fail_conservative();
        let relation = plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(relation.attempts, 2);
        assert_eq!(relation.completed, 2);
        assert_eq!(relation.objects_accessed, 2);
        assert!(relation
            .trace
            .iter()
            .all(|entry| entry.outcome == AccessOutcome::Failed));

        let (plan, rooted) = make_bound();
        let mut directory = prepare_observation_directory_membership_for_test(&rooted).unwrap();
        let mut first_token = None;
        for index in 0..directory.config().max_entries {
            let relative = PathBuf::from(format!("child-{index}.jsonl"));
            let child = directory
                .reserve_entry(&rooted, &relative)
                .unwrap()
                .complete(DirectoryEntryKind::File)
                .unwrap();
            first_token.get_or_insert(child.object_token());
        }
        assert_eq!(directory.accounted_entries(), 7);
        let excess_error = directory
            .reserve_entry(&rooted, PathBuf::from("child-7.jsonl").as_path())
            .unwrap_err();
        assert_eq!(
            excess_error.to_string(),
            "access relation descendant-objects exceeded MaxObjects"
        );
        assert!(!excess_error.to_string().contains("child-7.jsonl"));
        assert!(directory.is_failed());
        assert_eq!(directory.accounted_entries(), 7);
        rooted.fail_conservative();
        let relation = plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(relation.attempts, 9);
        assert_eq!(relation.denied, 1);
        assert_eq!(relation.objects_accessed, 8);

        let (plan, rooted) = make_bound();
        let mut directory = prepare_observation_directory_membership_for_test(&rooted).unwrap();
        let replayed = directory
            .reserve_entry(&rooted, PathBuf::from("child-0.jsonl").as_path())
            .unwrap()
            .complete(DirectoryEntryKind::File)
            .unwrap();
        assert_eq!(Some(replayed.object_token()), first_token);
        rooted.complete(512, AccessOutcome::Available).unwrap();
        assert!(plan.report().verify_digest());
    }

    #[cfg(unix)]
    #[test]
    fn authorized_directory_scan_mints_exact_refresh_bound_membership_evidence() {
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        let registry = AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![fixture_descendant_runtime_stream()])
                    .with_dependency_free_bootstrap_failure(PathBuf::from("nested/child.jsonl")),
            )
            .build_supported(catalog)
            .unwrap();
        let temp = TempDir::new().unwrap();
        let native_root = temp.path().join("authorized-root");
        let listing_root = native_root.join("sessions/secret-session-id/children");
        std::fs::create_dir_all(listing_root.join("nested")).unwrap();
        std::fs::write(listing_root.join("root.jsonl"), b"root").unwrap();
        std::fs::write(listing_root.join("nested/child.jsonl"), b"child").unwrap();
        let request = scoped_access_request(native_root.clone());
        let instance = request.source_instance.clone();
        let expected_source_instance_key = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        )
        .unwrap();
        let approved_root = ScopedAccessRootGrant {
            access_root: "root".to_string(),
            root: native_root,
        };
        let adapter = registry.get(&AdapterId::new("fixture").unwrap()).unwrap();
        let make_bound = |native_session_id: &[u8]| {
            let (_, authorization) = registry
                .authorize_typed_access(
                    &AdapterId::new("fixture").unwrap(),
                    &request.artifact_probe,
                    SupportOperation::ScopedTypedObservation,
                    &request.observation_contract_request.contract_versions,
                    &request.observation_contract_offer.contract_versions,
                )
                .unwrap();
            let plan = AuthorizedScopeAccessPlan::from_authorized_program(
                authorization
                    .select_scope_program("observe-session")
                    .unwrap(),
            )
            .unwrap();
            let identity = [ScopeIdentityInput {
                name: "native-session-id",
                value: native_session_id,
            }];
            let runtime = plan
                .reserve_observation_source(ScopeAccessRequest {
                    relation_id: "descendant-objects",
                    operation: AccessOperation::ObjectListing,
                    phase: AccessPhase::Initial,
                    parent_token: None,
                    identity_inputs: &identity,
                    depth: 1,
                    max_bytes: 1_024,
                    max_rows: 0,
                })
                .unwrap()
                .bind_runtime_stream(adapter.as_ref(), &instance)
                .unwrap();
            let rooted = bind_observation_runtime_source_for_test(
                runtime,
                Arc::clone(adapter),
                Arc::new(instance.clone()),
                &approved_root,
                &expected_source_instance_key,
            )
            .unwrap();
            (plan, rooted)
        };

        let (first_plan, first_binding) = make_bound(b"secret-session-id");
        let first =
            match scan_observation_directory_membership_for_test(first_binding, None).unwrap() {
                ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                other => panic!("expected an exact initial listing, got {other:?}"),
            };
        assert_eq!(first.accounted_entry_count(), 3);
        assert_eq!(first.selected_entry_count(), 2);
        assert_eq!(first.change_count(), 2);
        assert!(!first.root_moved());
        let first_revision = first.checkpoint().revision;
        let rendered = format!("{first:?}");
        for private in [
            "secret-session-id",
            "root.jsonl",
            "child.jsonl",
            "nested",
            "authorized-root",
        ] {
            assert!(!rendered.contains(private));
        }
        let first_relation = first_plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(first_relation.attempts, 4);
        assert_eq!(first_relation.completed, 4);
        assert_eq!(first_relation.objects_accessed, 4);
        assert!(first_relation
            .trace
            .iter()
            .all(|entry| entry.outcome == AccessOutcome::Available));

        std::fs::write(listing_root.join("root.jsonl"), b"root-expanded").unwrap();
        let (refresh_plan, refresh_binding) = make_bound(b"secret-session-id");
        let mut refreshed =
            match scan_observation_directory_membership_for_test(refresh_binding, Some(&first))
                .unwrap()
            {
                ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                other => panic!("expected an exact refreshed listing, got {other:?}"),
            };
        assert_eq!(refreshed.accounted_entry_count(), 3);
        assert_eq!(refreshed.selected_entry_count(), 2);
        assert_eq!(refreshed.change_count(), 1);
        assert!(!refreshed.root_moved());
        assert_ne!(refreshed.checkpoint().revision, first_revision);
        assert_eq!(refreshed.checkpoint().generation, 1);
        let refreshed_membership_revision = refreshed.checkpoint().revision;
        let refreshed_entries = refreshed.checkpoint().entries.clone();
        assert!(!refreshed.member_reads_complete());
        let mut member_bytes = Vec::new();
        while let Some(read) = refreshed.read_next_member().unwrap() {
            match read {
                ScopedObservationDirectoryMemberRead::Stable(content) => {
                    assert_ne!(content.listing_revision(), content.content_revision());
                    assert!(!format!("{content:?}").contains("root-expanded"));
                    let _object_token = content.object_token();
                    let expected_relative = if content.bytes() == b"child" {
                        std::path::Path::new(
                            "sessions/secret-session-id/children/nested/child.jsonl",
                        )
                    } else {
                        std::path::Path::new("sessions/secret-session-id/children/root.jsonl")
                    };
                    let expected_member_relative = if content.bytes() == b"child" {
                        std::path::Path::new("nested/child.jsonl")
                    } else {
                        std::path::Path::new("root.jsonl")
                    };
                    let expected_entry = refreshed_entries
                        .get(&confined_relative_path_key(expected_member_relative).unwrap())
                        .unwrap();
                    let expected_object_key =
                        confined_relative_path_key(expected_relative).unwrap();
                    let expected_stream_key =
                        CoverageStreamKey::derive("fixture", b"descendant-stream").unwrap();
                    let expected_coverage_object =
                        CoverageObjectKey::derive("descendant-stream", &expected_object_key)
                            .unwrap();
                    let identity = content.identity();
                    let identity_rendered = format!("{identity:?}");
                    for private in [
                        "secret-session-id",
                        "root.jsonl",
                        "child.jsonl",
                        "nested",
                        "descendant-objects",
                    ] {
                        assert!(!identity_rendered.contains(private));
                    }
                    assert_eq!(identity.relation_id(), "descendant-objects");
                    assert_eq!(identity.listing_generation(), 1);
                    assert_eq!(identity.listing_revision(), refreshed_membership_revision);
                    assert_eq!(identity.entry_generation(), expected_entry.generation);
                    assert_eq!(identity.entry_revision(), expected_entry.revision);
                    assert_eq!(identity.source().adapter_id.as_str(), "fixture");
                    assert_eq!(
                        identity.source().source_instance_key,
                        expected_source_instance_key
                    );
                    assert_eq!(identity.source().stream_key, expected_stream_key);
                    assert_eq!(identity.source().object_key, expected_coverage_object);
                    assert_eq!(
                        identity.semantic_context().source_instance_key(),
                        expected_source_instance_key
                    );
                    assert_eq!(
                        identity.semantic_context().stream_key(),
                        b"descendant-stream"
                    );
                    assert_eq!(
                        identity.semantic_context().object_key(),
                        expected_object_key
                    );
                    assert_eq!(
                        identity.semantic_context(),
                        &FactSemanticContext::new(
                            &AdapterId::new("fixture").unwrap(),
                            instance.spec.identity_contract_version,
                            instance.spec.stable_key.as_bytes(),
                            b"descendant-stream",
                            &expected_object_key,
                            1,
                        )
                        .unwrap()
                    );
                    assert_eq!(
                        content.runtime_stream_for_test(),
                        &fixture_descendant_runtime_stream()
                    );
                    assert!(Arc::ptr_eq(content.adapter_for_test(), adapter));
                    assert_eq!(content.source_instance_for_test().id, instance.id);
                    assert_eq!(
                        content.source_instance_for_test().spec.stable_key,
                        instance.spec.stable_key
                    );
                    assert_eq!(
                        content.descriptor_for_test().stream_id.as_str(),
                        "descendant-stream"
                    );
                    assert_eq!(
                        content.descriptor_for_test().object_key,
                        expected_object_key
                    );
                    assert_eq!(
                        content.descriptor_for_test().relative_path,
                        expected_relative
                    );
                    let expected_member_source = identity.source().clone();
                    let content_revision = content.content_revision();
                    if content.bytes() == b"child" {
                        let failure = content.bootstrap_for_test().unwrap_err();
                        assert_eq!(
                            failure.class_for_test(),
                            ScopedDecodeFailureClass::StreamFatal
                        );
                        let failure_rendered = format!("{failure:?}");
                        for private in [
                            "/Users/",
                            "alice",
                            "secret-session-id",
                            "child.jsonl",
                            "nested",
                            "descendant-stream",
                        ] {
                            assert!(!failure_rendered.contains(private));
                        }
                        let retained = failure.into_content_for_test();
                        assert_eq!(retained.content_revision(), content_revision);
                        assert_eq!(retained.identity().source(), &expected_member_source);
                        assert_eq!(retained.bytes(), b"child");
                        member_bytes.push(retained.bytes().to_vec());
                        continue;
                    }
                    let decoded_input = content.bootstrap_for_test().unwrap();
                    assert_eq!(
                        decoded_input.identity_for_test().source(),
                        &expected_member_source
                    );
                    assert_eq!(
                        decoded_input.runtime_stream_for_test(),
                        &fixture_descendant_runtime_stream()
                    );
                    assert_eq!(
                        decoded_input.descriptor_for_test().relative_path,
                        expected_relative
                    );
                    assert_eq!(decoded_input.object_context_for_test().version(), 1);
                    assert!(decoded_input.object_context_for_test().payload().is_empty());
                    assert_eq!(decoded_input.content_revision_for_test(), content_revision);
                    let decoded_rendered = format!("{decoded_input:?}");
                    for private in [
                        "secret-session-id",
                        "root.jsonl",
                        "child.jsonl",
                        "nested",
                        "descendant-stream",
                    ] {
                        assert!(!decoded_rendered.contains(private));
                    }
                    let origin = RecordOrigin {
                        source_instance_id: instance.id,
                        stream_id: 41,
                        object_id: 42,
                        observed_at: 43,
                        source_timestamp_hint: None,
                        media_type: SourceMediaType::new("application/json").unwrap(),
                    };
                    let wrong_origin = RecordOrigin {
                        source_instance_id: instance.id + 1,
                        ..origin.clone()
                    };
                    let frame_failure = decoded_input
                        .frame_initial_replace_for_test(wrong_origin)
                        .unwrap_err();
                    assert_eq!(
                        frame_failure.class_for_test(),
                        ScopedSourceFailureClass::InvalidCursor
                    );
                    let frame_failure_rendered = format!("{frame_failure:?}");
                    for private in [
                        "secret-session-id",
                        "root.jsonl",
                        "nested",
                        "descendant-stream",
                        "root-expanded",
                    ] {
                        assert!(!frame_failure_rendered.contains(private));
                    }
                    let decoded_input = frame_failure.into_input_for_test();
                    assert_eq!(decoded_input.bytes_for_test(), b"root-expanded");
                    let framed = decoded_input
                        .frame_initial_replace_for_test(origin)
                        .unwrap();
                    assert_eq!(framed.identity_for_test().source(), &expected_member_source);
                    assert_eq!(
                        framed.runtime_stream_for_test(),
                        &fixture_descendant_runtime_stream()
                    );
                    assert_eq!(
                        framed.descriptor_for_test().relative_path,
                        expected_relative
                    );
                    assert_eq!(framed.object_context_for_test().version(), 1);
                    let checkpoint = framed.checkpoint_for_test();
                    assert_eq!(checkpoint.generation, 1);
                    assert!(checkpoint.present);
                    assert_eq!(checkpoint.revision, content_revision);
                    let record = framed.record_for_test();
                    assert_eq!(record.generation, 1);
                    assert_eq!(record.cursor_start, SourceCursor::snapshot(Revision::ZERO));
                    assert_eq!(record.cursor_end, checkpoint.cursor());
                    assert_eq!(record.payload, b"root-expanded");
                    let framed_rendered = format!("{framed:?}");
                    for private in [
                        "secret-session-id",
                        "root.jsonl",
                        "nested",
                        "descendant-stream",
                        "root-expanded",
                    ] {
                        assert!(!framed_rendered.contains(private));
                    }
                    member_bytes.push(record.payload.clone());
                }
                other => panic!("expected a stable selected member, got {other:?}"),
            }
        }
        member_bytes.sort();
        assert_eq!(
            member_bytes,
            vec![b"child".to_vec(), b"root-expanded".to_vec()]
        );
        assert!(refreshed.member_reads_complete());
        assert!(refreshed.read_next_member().unwrap().is_none());
        assert!(refresh_plan.report().verify_digest());
        let refreshed_relation = refresh_plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(refreshed_relation.attempts, 6);
        assert_eq!(refreshed_relation.completed, 6);
        assert_eq!(refreshed_relation.bytes_read, 18);
        assert!(refreshed_relation.trace[4..]
            .iter()
            .all(|entry| entry.operation == AccessOperation::ObjectRead
                && entry.reserved_bytes == 1_024
                && entry.outcome == AccessOutcome::Available));
        let (_refresh_verify_plan, refresh_verify_binding) = make_bound(b"secret-session-id");
        let refresh_verification = match scan_observation_directory_membership_for_test(
            refresh_verify_binding,
            Some(&refreshed),
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected a final refresh verification snapshot, got {other:?}"),
        };
        refreshed
            .confirm_membership_unchanged(refresh_verification)
            .unwrap();
        let observation =
            ScopedRelationMembershipObservation::from_directory_listing(refreshed).unwrap();

        let (capacity_plan, capacity_binding) = make_bound(b"secret-session-id");
        let mut capacity_listing =
            match scan_observation_directory_membership_for_test(capacity_binding, None).unwrap() {
                ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                other => panic!("expected an exact capacity listing, got {other:?}"),
            };
        while capacity_listing.read_next_member().unwrap().is_some() {}
        assert!(capacity_listing.member_reads_complete());
        let (_capacity_verify_plan, capacity_verify_binding) = make_bound(b"secret-session-id");
        let capacity_verification = match scan_observation_directory_membership_for_test(
            capacity_verify_binding,
            Some(&capacity_listing),
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected a capacity verification snapshot, got {other:?}"),
        };
        capacity_listing
            .confirm_membership_unchanged(capacity_verification)
            .unwrap();
        let capacity_observation =
            ScopedRelationMembershipObservation::from_directory_listing(capacity_listing).unwrap();
        let mut undersized_admission = admission_lane_for_objects(2);
        assert_eq!(
            undersized_admission.record_relation_membership(
                7,
                ScopedAppendDeliveryPhase::Correction,
                capacity_observation,
            ),
            Err(ScopedAdmissionError::CoverageObjectCapacityFull)
        );
        assert!(capacity_plan.report().verify_digest());

        let race_root = approved_root.root.join("sessions/race-session-id/children");
        std::fs::create_dir_all(&race_root).unwrap();
        let race_member = race_root.join("victim.jsonl");
        std::fs::write(&race_member, b"before").unwrap();
        let (race_plan, race_binding) = make_bound(b"race-session-id");
        let mut race_listing =
            match scan_observation_directory_membership_for_test(race_binding, None).unwrap() {
                ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                other => panic!("expected a race listing, got {other:?}"),
            };
        std::fs::remove_file(&race_member).unwrap();
        std::fs::write(&race_member, b"replacement").unwrap();
        assert!(matches!(
            race_listing.read_next_member().unwrap(),
            Some(ScopedObservationDirectoryMemberRead::RetryTransient)
        ));
        assert!(!race_listing.member_reads_complete());
        assert!(ScopedRelationMembershipObservation::from_directory_listing(race_listing).is_err());
        let race_relation = race_plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(
            race_relation.trace.last().unwrap().operation,
            AccessOperation::ObjectRead
        );
        assert_eq!(
            race_relation.trace.last().unwrap().outcome,
            AccessOutcome::Failed
        );

        let late_root = approved_root
            .root
            .join("sessions/late-member-session-id/children");
        std::fs::create_dir_all(&late_root).unwrap();
        std::fs::write(late_root.join("first.jsonl"), b"first").unwrap();
        let (late_plan, late_binding) = make_bound(b"late-member-session-id");
        let mut late_listing =
            match scan_observation_directory_membership_for_test(late_binding, None).unwrap() {
                ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                other => panic!("expected a late-member listing, got {other:?}"),
            };
        while late_listing.read_next_member().unwrap().is_some() {}
        assert!(late_listing.member_reads_complete());
        std::fs::write(late_root.join("created-after-read.jsonl"), b"late").unwrap();
        let (_late_verify_plan, late_verify_binding) = make_bound(b"late-member-session-id");
        let late_verification = match scan_observation_directory_membership_for_test(
            late_verify_binding,
            Some(&late_listing),
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected a changed late-member snapshot, got {other:?}"),
        };
        assert!(late_listing
            .confirm_membership_unchanged(late_verification)
            .is_err());
        assert!(ScopedRelationMembershipObservation::from_directory_listing(late_listing).is_err());
        assert!(late_plan.report().verify_digest());

        let oversized_root = approved_root
            .root
            .join("sessions/oversized-session-id/children");
        std::fs::create_dir_all(&oversized_root).unwrap();
        std::fs::write(oversized_root.join("large.jsonl"), vec![b'x'; 1_025]).unwrap();
        let (oversized_plan, oversized_binding) = make_bound(b"oversized-session-id");
        let mut oversized_listing = match scan_observation_directory_membership_for_test(
            oversized_binding,
            None,
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
            other => panic!("expected an oversized listing, got {other:?}"),
        };
        let Some(ScopedObservationDirectoryMemberRead::Oversized {
            binding: oversized_member,
        }) = oversized_listing.read_next_member().unwrap()
        else {
            panic!("expected one stable oversized member")
        };
        assert_eq!(
            oversized_member.runtime_stream_for_test(),
            &fixture_descendant_runtime_stream()
        );
        assert!(Arc::ptr_eq(oversized_member.adapter_for_test(), adapter));
        assert_eq!(oversized_member.source_instance_for_test().id, instance.id);
        assert_eq!(
            oversized_member.descriptor_for_test().relative_path,
            std::path::Path::new("sessions/oversized-session-id/children/large.jsonl")
        );
        let oversized_rendered = format!("{oversized_member:?}");
        for private in ["oversized-session-id", "large.jsonl", "descendant-stream"] {
            assert!(!oversized_rendered.contains(private));
        }
        assert!(oversized_listing.member_reads_complete());
        let (_oversized_verify_plan, oversized_verify_binding) =
            make_bound(b"oversized-session-id");
        let oversized_verification = match scan_observation_directory_membership_for_test(
            oversized_verify_binding,
            Some(&oversized_listing),
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected an oversized verification snapshot, got {other:?}"),
        };
        oversized_listing
            .confirm_membership_unchanged(oversized_verification)
            .unwrap();
        assert!(
            ScopedRelationMembershipObservation::from_directory_listing(oversized_listing).is_ok()
        );
        let oversized_relation = oversized_plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(
            oversized_relation.trace.last().unwrap().outcome,
            AccessOutcome::Oversized
        );
        assert_eq!(oversized_relation.trace.last().unwrap().bytes_read, 0);

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;

            let non_utf8_root = approved_root
                .root
                .join("sessions/non-utf8-session-id/children");
            std::fs::create_dir_all(&non_utf8_root).unwrap();
            let non_utf8_name = std::ffi::OsString::from_vec(vec![
                b'c', b'h', b'i', b'l', b'd', 0xff, b'.', b'j', b's', b'o', b'n', b'l',
            ]);
            std::fs::write(non_utf8_root.join(non_utf8_name), b"binary-name").unwrap();
            let (_non_utf8_plan, non_utf8_binding) = make_bound(b"non-utf8-session-id");
            let mut non_utf8_listing =
                match scan_observation_directory_membership_for_test(non_utf8_binding, None)
                    .unwrap()
                {
                    ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                    other => panic!("expected a non-UTF-8 listing, got {other:?}"),
                };
            let Some(ScopedObservationDirectoryMemberRead::Stable(non_utf8_content)) =
                non_utf8_listing.read_next_member().unwrap()
            else {
                panic!("expected one stable non-UTF-8 member")
            };
            assert_eq!(non_utf8_content.bytes(), b"binary-name");
            assert!(non_utf8_listing.read_next_member().unwrap().is_none());
            assert!(non_utf8_listing.member_reads_complete());
        }

        let (substituted_plan, substituted_binding) = make_bound(b"other-session-id");
        let error =
            scan_observation_directory_membership_for_test(substituted_binding, Some(&first))
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "scoped observation source binding does not match the active attachment"
        );
        let substituted_relation = substituted_plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(substituted_relation.attempts, 1);
        assert_eq!(substituted_relation.completed, 1);
        assert_eq!(substituted_relation.trace[0].outcome, AccessOutcome::Failed);

        let (foreign_attachment_plan, foreign_attachment_binding) =
            make_bound(b"secret-session-id");
        let error = scan_observation_directory_membership_with_foreign_attachment_for_test(
            foreign_attachment_binding,
            &first,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "scoped observation source binding does not match the active attachment"
        );
        let foreign_attachment_relation = foreign_attachment_plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(foreign_attachment_relation.attempts, 1);
        assert_eq!(foreign_attachment_relation.completed, 1);
        assert_eq!(
            foreign_attachment_relation.trace[0].outcome,
            AccessOutcome::Failed
        );

        let (missing_plan, missing_binding) = make_bound(b"missing-session-id");
        assert!(matches!(
            scan_observation_directory_membership_for_test(missing_binding, None).unwrap(),
            ScopedObservationDirectoryScan::Unavailable
        ));
        let missing_relation = missing_plan
            .report()
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap()
            .clone();
        assert_eq!(missing_relation.attempts, 1);
        assert_eq!(missing_relation.completed, 1);
        assert_eq!(
            missing_relation.trace[0].outcome,
            AccessOutcome::Unavailable
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            symlink(
                "/Users/alice/private/secret.jsonl",
                listing_root.join("private-link.jsonl"),
            )
            .unwrap();
            let (failed_plan, failed_binding) = make_bound(b"secret-session-id");
            let error =
                scan_observation_directory_membership_for_test(failed_binding, Some(&first))
                    .unwrap_err();
            assert_eq!(
                error.to_string(),
                "scoped observation directory scan failed"
            );
            for private in ["/Users/", "alice", "private", "secret.jsonl"] {
                assert!(!error.to_string().contains(private));
            }
            let failed_relation = failed_plan
                .report()
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "descendant-objects")
                .unwrap()
                .clone();
            assert!(failed_relation.attempts >= 2);
            assert!(failed_relation
                .trace
                .iter()
                .any(|entry| entry.outcome == AccessOutcome::Failed));

            let link_root = approved_root
                .root
                .join("sessions/read-link-session-id/children");
            std::fs::create_dir_all(&link_root).unwrap();
            let link_member = link_root.join("selected.jsonl");
            let outside = temp.path().join("outside-private.jsonl");
            std::fs::write(&link_member, b"inside").unwrap();
            std::fs::write(&outside, b"outside-private-bytes").unwrap();
            let (link_plan, link_binding) = make_bound(b"read-link-session-id");
            let mut link_listing =
                match scan_observation_directory_membership_for_test(link_binding, None).unwrap() {
                    ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                    other => panic!("expected a pre-link listing, got {other:?}"),
                };
            std::fs::remove_file(&link_member).unwrap();
            symlink(&outside, &link_member).unwrap();
            let error = link_listing.read_next_member().unwrap_err();
            assert_eq!(
                error.to_string(),
                "scoped observation directory scan failed"
            );
            for private in ["outside-private", "selected.jsonl", "outside-private-bytes"] {
                assert!(!error.to_string().contains(private));
            }
            assert!(!link_listing.member_reads_complete());
            assert!(
                ScopedRelationMembershipObservation::from_directory_listing(link_listing).is_err()
            );
            let link_relation = link_plan
                .report()
                .relations()
                .iter()
                .find(|relation| relation.relation_id == "descendant-objects")
                .unwrap()
                .clone();
            assert_eq!(
                link_relation.trace.last().unwrap().operation,
                AccessOperation::ObjectRead
            );
            assert_eq!(
                link_relation.trace.last().unwrap().outcome,
                AccessOutcome::Failed
            );
        }

        let mut admission = admission_lane_for_objects(3);
        admission
            .record_relation_membership(7, ScopedAppendDeliveryPhase::Correction, observation)
            .unwrap();
        assert!(ScopedRelationMembershipObservation::from_directory_listing(first).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nested_directory_child_jsonl_decodes_through_replace_driver() {
        let nested_jsonl = b"{\"type\":\"nested-child\"}\n";
        let (catalog, binding, scope_programs) =
            promoted_fixture_catalog_with_scope(UNCOMPOSED_DYNAMIC_SCOPE_DOCUMENT);
        let registry = AdapterRegistryBuilder::new()
            .register(
                EmptyAdapter::new("fixture")
                    .with_support(binding, scope_programs)
                    .with_streams(vec![fixture_descendant_runtime_stream()])
                    .with_stateful_decode(),
            )
            .build_supported(catalog)
            .unwrap();
        let temp = TempDir::new().unwrap();
        let native_root = temp.path().join("authorized-root");
        let listing_root = native_root.join("sessions/secret-session-id/children");
        std::fs::create_dir_all(listing_root.join("nested")).unwrap();
        std::fs::write(listing_root.join("nested/child.jsonl"), nested_jsonl).unwrap();
        let request = scoped_access_request(native_root.clone());
        let instance = request.source_instance.clone();
        let expected_source_instance_key = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        )
        .unwrap();
        let approved_root = ScopedAccessRootGrant {
            access_root: "root".to_string(),
            root: native_root,
        };
        let adapter = registry.get(&AdapterId::new("fixture").unwrap()).unwrap();
        let origin = RecordOrigin {
            source_instance_id: instance.id,
            stream_id: 41,
            object_id: 42,
            observed_at: 43,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let make_bound = |native_session_id: &[u8]| {
            let (_, authorization) = registry
                .authorize_typed_access(
                    &AdapterId::new("fixture").unwrap(),
                    &request.artifact_probe,
                    SupportOperation::ScopedTypedObservation,
                    &request.observation_contract_request.contract_versions,
                    &request.observation_contract_offer.contract_versions,
                )
                .unwrap();
            let plan = AuthorizedScopeAccessPlan::from_authorized_program(
                authorization
                    .select_scope_program("observe-session")
                    .unwrap(),
            )
            .unwrap();
            let identity = [ScopeIdentityInput {
                name: "native-session-id",
                value: native_session_id,
            }];
            let runtime = plan
                .reserve_observation_source(ScopeAccessRequest {
                    relation_id: "descendant-objects",
                    operation: AccessOperation::ObjectListing,
                    phase: AccessPhase::Initial,
                    parent_token: None,
                    identity_inputs: &identity,
                    depth: 1,
                    max_bytes: 1_024,
                    max_rows: 0,
                })
                .unwrap()
                .bind_runtime_stream(adapter.as_ref(), &instance)
                .unwrap();
            let rooted = bind_observation_runtime_source_for_test(
                runtime,
                Arc::clone(adapter),
                Arc::new(instance.clone()),
                &approved_root,
                &expected_source_instance_key,
            )
            .unwrap();
            (plan, rooted)
        };

        let (present_plan, present_binding) = make_bound(b"secret-session-id");
        let mut present_listing =
            match scan_observation_directory_membership_for_test(present_binding, None).unwrap() {
                ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                other => panic!("expected a nested listing, got {other:?}"),
            };
        assert_eq!(present_listing.selected_entry_count(), 1);
        let Some(ScopedObservationDirectoryMemberRead::Stable(content)) =
            present_listing.read_next_member().unwrap()
        else {
            panic!("expected one stable nested jsonl child")
        };
        assert_eq!(content.bytes(), nested_jsonl);
        assert!(present_listing.member_reads_complete());
        let decoded_input = content.bootstrap_for_test().unwrap();
        let wrong_origin = RecordOrigin {
            source_instance_id: instance.id + 1,
            ..origin.clone()
        };
        let observe_failure = present_listing
            .observe_bootstrapped_member(decoded_input, &wrong_origin, None)
            .unwrap_err();
        assert_eq!(
            observe_failure.kind_for_test(),
            ScopedObservationDirectoryMemberObserveFailureKind::Source(
                ScopedSourceFailureClass::InvalidCursor
            )
        );
        let observe_failure_rendered = format!("{observe_failure:?}");
        for private in ["secret-session-id", "child.jsonl", "nested-child"] {
            assert!(!observe_failure_rendered.contains(private));
        }
        let decoded_input = observe_failure.into_input_for_test();
        let nested = present_listing
            .observe_bootstrapped_member(decoded_input, &origin, None)
            .unwrap();
        let nested_rendered = format!("{nested:?}");
        for private in [
            "secret-session-id",
            "child.jsonl",
            "nested",
            "nested-child",
            "descendant-stream",
            "authorized-root",
        ] {
            assert!(!nested_rendered.contains(private));
        }
        let snapshot = nested
            .present_snapshot_for_test()
            .expect("present nested jsonl must decode");
        assert_eq!(
            snapshot.disposition_for_test(),
            DecodeDisposition::PreservedUnknown
        );
        assert_eq!(snapshot.fact_count_for_test(), 1);
        assert_eq!(snapshot.record_payload_for_test(), nested_jsonl);
        assert_eq!(
            snapshot.next_decoder_state_for_test(),
            Some(nested_jsonl.as_slice())
        );
        match &snapshot.facts_for_test()[0].value {
            Fact::UnknownRecord {
                native_kind,
                raw_payload,
                reason,
            } => {
                assert_eq!(native_kind.as_deref(), Some("fixture"));
                assert!(raw_payload.is_empty());
                assert_eq!(reason, "fixture");
            }
            other => panic!("expected a decoded unknown record, got {other:?}"),
        }
        let nested_source = nested.source().clone();
        let (_present_verify_plan, present_verify_binding) = make_bound(b"secret-session-id");
        let present_verification = match scan_observation_directory_membership_for_test(
            present_verify_binding,
            Some(&present_listing),
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected a present-member verification snapshot, got {other:?}"),
        };
        present_listing
            .confirm_membership_unchanged(present_verification)
            .unwrap();
        let observation =
            ScopedRelationMembershipObservation::from_directory_listing(present_listing).unwrap();
        let mut admission = admission_lane_for_objects(2);
        admission
            .record_relation_membership(7, ScopedAppendDeliveryPhase::Bootstrap, observation)
            .unwrap();
        assert!(admission
            .directory_relation_listing("descendant-objects")
            .is_some());
        let receipt = admission
            .admit_directory_member(7, ScopedAppendDeliveryPhase::Bootstrap, nested)
            .unwrap();
        assert_eq!(
            admission.directory_member_decoder_state(&nested_source),
            Some(nested_jsonl.as_slice())
        );
        assert_eq!(receipt.data_events, 1);
        assert_eq!(receipt.retained_native_bytes, 0);
        match admission.pop_next() {
            Some(ScopedQueuedObservationFrame::Decoded { item, source, .. }) => {
                assert_eq!(source, nested_source);
                match *item {
                    ScopedDecodedAppendItem::Record {
                        disposition, batch, ..
                    } => {
                        assert_eq!(disposition, DecodeDisposition::PreservedUnknown);
                        assert_eq!(batch.facts().len(), 1);
                    }
                    ScopedDecodedAppendItem::DriverQuarantine(_) => {
                        panic!("expected admitted decoded facts, got a driver quarantine")
                    }
                }
            }
            None => panic!("expected one admitted decoded frame"),
            Some(_) => panic!("expected one admitted decoded frame"),
        }
        assert!(admission.pop_next().is_none());
        assert!(present_plan.report().verify_digest());

        let retained_root = approved_root
            .root
            .join("sessions/retained-child-session-id/children");
        std::fs::create_dir_all(retained_root.join("nested")).unwrap();
        let retained_member = retained_root.join("nested/child.jsonl");
        std::fs::write(&retained_member, nested_jsonl).unwrap();
        let (retained_plan, retained_binding) = make_bound(b"retained-child-session-id");
        let mut retained_listing =
            match scan_observation_directory_membership_for_test(retained_binding, None).unwrap() {
                ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
                other => panic!("expected a retained-child listing, got {other:?}"),
            };
        let Some(ScopedObservationDirectoryMemberRead::Stable(retained_content)) =
            retained_listing.read_next_member().unwrap()
        else {
            panic!("expected one stable child before deletion")
        };
        let retained_input = retained_content.bootstrap_for_test().unwrap();
        std::fs::remove_file(&retained_member).unwrap();
        let retained_lifecycle = retained_listing
            .observe_bootstrapped_member(retained_input, &origin, Some(b"retained-prior-state:"))
            .unwrap();
        assert_eq!(retained_lifecycle.absence_for_test(), None);
        assert_eq!(
            retained_lifecycle
                .present_snapshot_for_test()
                .expect("the authorized stable read remains the decode snapshot")
                .record_payload_for_test(),
            nested_jsonl
        );
        assert_eq!(
            retained_lifecycle
                .present_snapshot_for_test()
                .unwrap()
                .next_decoder_state_for_test(),
            Some(
                [b"retained-prior-state:".as_slice(), nested_jsonl.as_slice()]
                    .concat()
                    .as_slice()
            )
        );
        let retained_rendered = format!("{retained_lifecycle:?}");
        for private in ["retained-child-session-id", "child.jsonl", "nested-child"] {
            assert!(!retained_rendered.contains(private));
        }
        let (_retained_verify_plan, retained_verify_binding) =
            make_bound(b"retained-child-session-id");
        let retained_verification = match scan_observation_directory_membership_for_test(
            retained_verify_binding,
            Some(&retained_listing),
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected a changed retained-member snapshot, got {other:?}"),
        };
        assert!(retained_listing
            .confirm_membership_unchanged(retained_verification)
            .is_err());
        assert!(
            ScopedRelationMembershipObservation::from_directory_listing(retained_listing).is_err()
        );
        let mut retained_admission = admission_lane_for_objects(2);
        let retained_failure = retained_admission
            .admit_directory_member(8, ScopedAppendDeliveryPhase::Bootstrap, retained_lifecycle)
            .unwrap_err();
        assert_eq!(
            retained_failure.error,
            ScopedAdmissionError::InvalidCoverage
        );
        assert!(retained_admission.pop_next().is_none());
        let retained_report = retained_plan.report();
        let retained_relation = retained_report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "descendant-objects")
            .unwrap();
        assert_eq!(
            retained_relation
                .trace
                .iter()
                .filter(|entry| entry.operation == AccessOperation::ObjectRead)
                .count(),
            1,
            "decode must not perform an unreserved second native read"
        );
        assert!(retained_report.verify_digest());

        let oversized_root = approved_root
            .root
            .join("sessions/oversized-child-session-id/children");
        std::fs::create_dir_all(&oversized_root).unwrap();
        std::fs::write(oversized_root.join("large.jsonl"), vec![b'x'; 1_025]).unwrap();
        let (oversized_plan, oversized_binding) = make_bound(b"oversized-child-session-id");
        let mut oversized_listing = match scan_observation_directory_membership_for_test(
            oversized_binding,
            None,
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(listing) => *listing,
            other => panic!("expected an oversized listing, got {other:?}"),
        };
        let Some(ScopedObservationDirectoryMemberRead::Oversized {
            binding: oversized_member,
        }) = oversized_listing.read_next_member().unwrap()
        else {
            panic!("expected one oversized selected child")
        };
        let oversized_lifecycle = oversized_member.oversized_lifecycle();
        assert!(matches!(
            oversized_lifecycle,
            ScopedObservationDirectoryMemberLifecycle::Oversized { .. }
        ));
        let oversized_rendered = format!("{oversized_lifecycle:?}");
        for private in ["oversized-child-session-id", "large.jsonl"] {
            assert!(!oversized_rendered.contains(private));
        }
        let (_oversized_verify_plan, oversized_verify_binding) =
            make_bound(b"oversized-child-session-id");
        let oversized_verification = match scan_observation_directory_membership_for_test(
            oversized_verify_binding,
            Some(&oversized_listing),
        )
        .unwrap()
        {
            ScopedObservationDirectoryScan::Snapshot(verification) => *verification,
            other => panic!("expected an oversized-member verification snapshot, got {other:?}"),
        };
        oversized_listing
            .confirm_membership_unchanged(oversized_verification)
            .unwrap();
        let oversized_observation =
            ScopedRelationMembershipObservation::from_directory_listing(oversized_listing).unwrap();
        let mut oversized_admission = admission_lane_for_objects(2);
        oversized_admission
            .record_relation_membership(
                9,
                ScopedAppendDeliveryPhase::Bootstrap,
                oversized_observation,
            )
            .unwrap();
        let oversized_failure = oversized_admission
            .admit_directory_member(9, ScopedAppendDeliveryPhase::Bootstrap, oversized_lifecycle)
            .unwrap_err();
        assert_eq!(
            oversized_failure.error,
            ScopedAdmissionError::InvalidCoverage
        );
        assert!(oversized_admission.pop_next().is_none());
        let mut projection =
            ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
                max_usage_v2_entities: 1,
            })
            .unwrap();
        assert!(oversized_admission
            .project_next(&mut projection)
            .unwrap()
            .is_none());
        assert!(oversized_plan.report().verify_digest());

        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(approved_root.root.clone()),
        )
        .expect("directory composition attaches; bootstrap still requires membership");
        let admission = admission_lane_for_objects(8);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        let _ticket = host.request_poll().unwrap();
        let lease = host.begin_poll().unwrap().unwrap();
        assert!(matches!(
            host.complete_bootstrap_poll(lease, &admission, &projection, &drain),
            Err(ScopedObservationPollError::IncompleteScopePass)
        ));
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
        let expected_capabilities = negotiate_observation_contract(
            &request.observation_contract_request,
            &request.observation_contract_offer,
        )
        .unwrap();
        let expected_capability_report = ObservationCapabilities::from_negotiation(
            expected_capabilities.clone(),
            &request.observation_contract_offer,
            CompatibilityClass::ExactSupported,
            Some("fixture-release"),
            &[("runtime.usage-v2", 1)],
        )
        .unwrap();
        let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
        assert_eq!(host.contract_selection(), &expected_capabilities);
        assert_eq!(host.capabilities(), &expected_capability_report);
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

    #[test]
    fn scoped_artifact_access_policy_is_attachment_bound_and_fail_closed() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let mut request = scoped_access_request(temp.path().join("artifact-policy"));
        request.artifact_access_policy =
            ScopedArtifactAccessPolicy::bounded(1024, ScopedArtifactContentPolicy::HashOnly);
        let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
        assert_eq!(
            host.artifact_access_policy(),
            ScopedArtifactAccessPolicy::bounded(1024, ScopedArtifactContentPolicy::HashOnly)
        );
        let artifact_key = CanonicalEntityKey::derive(
            "fixture",
            &host.root_identity().source_instance_key,
            "artifact",
            b"policy-bound-artifact",
        )
        .unwrap();

        assert!(host
            .prepare_portable_artifact_read(
                artifact_key,
                "workflow_definition",
                None,
                1024,
                ScopedArtifactContentPolicy::MetadataOnly,
            )
            .is_ok());
        let command = host
            .prepare_portable_artifact_read(
                artifact_key,
                "workflow_definition",
                Some(1),
                1024,
                ScopedArtifactContentPolicy::HashOnly,
            )
            .unwrap();
        assert!(host.validate_portable_artifact_command(&command).is_ok());
        assert!(host
            .prepare_portable_artifact_read(
                artifact_key,
                "workflow_definition",
                Some(1),
                1024,
                ScopedArtifactContentPolicy::Inline,
            )
            .is_err());
        assert!(host
            .prepare_portable_artifact_read(
                artifact_key,
                "workflow_definition",
                None,
                1025,
                ScopedArtifactContentPolicy::MetadataOnly,
            )
            .is_err());

        let mut foreign_request =
            scoped_access_request(temp.path().join("artifact-policy-foreign"));
        foreign_request.artifact_access_policy =
            ScopedArtifactAccessPolicy::bounded(1024, ScopedArtifactContentPolicy::HashOnly);
        let foreign = ScopedObservationAccessHost::authorize(&registry, foreign_request).unwrap();
        assert!(foreign
            .validate_portable_artifact_command(&command)
            .is_err());

        let mut disabled_request =
            scoped_access_request(temp.path().join("artifact-policy-disabled"));
        disabled_request.artifact_access_policy = ScopedArtifactAccessPolicy::disabled();
        let disabled = ScopedObservationAccessHost::authorize(&registry, disabled_request).unwrap();
        assert!(disabled
            .prepare_portable_artifact_read(
                artifact_key,
                "workflow_definition",
                None,
                1,
                ScopedArtifactContentPolicy::MetadataOnly,
            )
            .is_err());

        let mut invalid_request =
            scoped_access_request(temp.path().join("artifact-policy-invalid"));
        invalid_request.artifact_access_policy =
            ScopedArtifactAccessPolicy::bounded(0, ScopedArtifactContentPolicy::MetadataOnly);
        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, invalid_request),
            Err(ScopedObservationAccessError::InvalidArtifactPolicy(_))
        ));
    }

    #[tokio::test]
    async fn scoped_portable_close_is_exact_attachment_bound_and_idempotent() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let first = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("portable-close-first")),
        )
        .unwrap();
        let second = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("portable-close-second")),
        )
        .unwrap();
        let limits = ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        };
        let mut first_drain = first.open_consumer_drain(limits).unwrap();
        let mut second_drain = second.open_consumer_drain(limits).unwrap();
        let first_command = first.prepare_portable_close().unwrap();
        let repeated_command = first.prepare_portable_close().unwrap();
        let second_command = second.prepare_portable_close().unwrap();
        assert_eq!(
            serde_json::to_value(first_command.context_wire()).unwrap(),
            serde_json::to_value(repeated_command.context_wire()).unwrap()
        );

        let foreign_command =
            match first.close_portable_with_consumer(&second_command, &mut first_drain) {
                Err(error) => error,
                Ok(_) => panic!("foreign close command must fail"),
            };
        assert!(foreign_command
            .to_string()
            .contains("another observer attachment"));
        assert!(!first.is_closed());
        assert!(!first_drain.is_closed());

        let foreign_drain =
            match first.close_portable_with_consumer(&first_command, &mut second_drain) {
                Err(error) => error,
                Ok(_) => panic!("foreign consumer drain must fail"),
            };
        assert!(foreign_drain
            .to_string()
            .contains("another observer attachment"));
        assert!(!first.is_closed());
        assert!(!second.is_closed());
        assert!(!second_drain.is_closed());

        let operation = first
            .close_portable_with_consumer(&first_command, &mut first_drain)
            .unwrap();
        let receipt = operation.wait_async().await.unwrap();
        let receipt_value = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serde_json::to_value(operation.parse_receipt(receipt_value.clone()).unwrap()).unwrap(),
            receipt_value
        );
        assert!(first.is_closed());
        assert!(first_drain.is_closed());

        let repeated = first
            .close_portable_with_consumer(&repeated_command, &mut first_drain)
            .unwrap();
        assert_eq!(
            serde_json::to_value(repeated.wait_async().await.unwrap()).unwrap(),
            receipt_value
        );

        let second_operation = second
            .close_portable_with_consumer(&second_command, &mut second_drain)
            .unwrap();
        assert!(second_operation.wait_async().await.is_ok());
    }

    #[test]
    fn scoped_host_rejects_incompatible_observation_contract_before_registry_authority() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let mut request = scoped_access_request(temp.path().to_path_buf());
        request.adapter_id = "not-registered".to_string();
        request.artifact_access_policy =
            ScopedArtifactAccessPolicy::bounded(0, ScopedArtifactContentPolicy::Inline);
        request.observation_contract_request.event_contract_versions = vec![2];

        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, request),
            Err(ScopedObservationAccessError::ObservationContract(
                ObservationNegotiationError::IncompatibleObservationContract {
                    axis: ObservationCompatibilityAxis::EventContractVersion,
                }
            ))
        ));
    }

    #[test]
    fn scoped_host_negotiates_and_retains_the_optional_unknown_wire_sidecar_pre_access() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();

        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("selected")),
        )
        .unwrap();
        let selected = host.unknown_wire_contract_selection().unwrap();
        assert_eq!(selected.max_preserved_bytes(), 4_096);
        assert_eq!(selected.observation_selection(), host.contract_selection());
        let drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        assert_eq!(drain.unknown_wire_contract_selection(), Some(selected));

        let mut absent = scoped_access_request(temp.path().join("absent"));
        absent.unknown_wire_contract = None;
        let absent = ScopedObservationAccessHost::authorize(&registry, absent).unwrap();
        assert!(absent.unknown_wire_contract_selection().is_none());
        let absent_drain = absent
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            })
            .unwrap();
        assert!(absent_drain.unknown_wire_contract_selection().is_none());

        let mut incompatible = scoped_access_request(temp.path().join("incompatible"));
        incompatible.adapter_id = "not-registered".to_owned();
        incompatible.artifact_access_policy =
            ScopedArtifactAccessPolicy::bounded(0, ScopedArtifactContentPolicy::Inline);
        let mut request = serde_json::to_value(
            ObservationUnknownWireContractRequest::new(
                ObservationUnknownWireCapability::preserving(8_192).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        request["capability"]["preserves_encoded_value"] = serde_json::json!(false);
        incompatible.unknown_wire_contract = Some(ScopedObservationUnknownWireNegotiation::new(
            serde_json::from_value(request).unwrap(),
            ObservationUnknownWireContractOffer::new(
                ObservationUnknownWireCapability::preserving(4_096).unwrap(),
            )
            .unwrap(),
        ));
        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, incompatible),
            Err(ScopedObservationAccessError::UnknownWireContract(
                ObservationUnknownWireContractError::Incompatible {
                    axis: ObservationUnknownWireCompatibilityAxis::EncodedValuePreservation,
                }
            ))
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
        let ScopedObservationContextualPollResolution::Ready(first_contextual) =
            host.contextual_poll_resolution(&first).unwrap()
        else {
            panic!("the first completed ticket must bind its contextual watermark");
        };
        let ScopedObservationContextualPollResolution::Ready(second_contextual) =
            host.contextual_poll_resolution(&second).unwrap()
        else {
            panic!("the coalesced ticket must bind its contextual watermark");
        };
        assert!(Arc::ptr_eq(
            first_contextual.watermark(),
            second_contextual.watermark()
        ));
        let first_wire = first_contextual.watermark_wire_value().unwrap();
        assert_eq!(
            first_wire,
            second_contextual.watermark_wire_value().unwrap()
        );
        assert!(!first_wire.to_string().contains("request_generation"));
        assert!(!first_contextual
            .context_wire_value()
            .unwrap()
            .to_string()
            .contains("request_generation"));
        assert_eq!(
            host.poll_resolution(&third).unwrap(),
            ScopedObservationPollResolution::Pending
        );
        assert!(matches!(
            host.contextual_poll_resolution(&third).unwrap(),
            ScopedObservationContextualPollResolution::Pending
        ));

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
        let ScopedObservationContextualPollResolution::Ready(third_contextual) =
            host.contextual_poll_resolution(&third).unwrap()
        else {
            panic!("the follow-up ticket must bind its own contextual watermark");
        };
        assert!(!Arc::ptr_eq(
            first_contextual.watermark(),
            third_contextual.watermark()
        ));
        assert_eq!(
            first_contextual.watermark_wire_value().unwrap(),
            third_contextual.watermark_wire_value().unwrap()
        );

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
                failed: false,
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
        assert!(matches!(
            second.contextual_poll_resolution(&ticket),
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
            first.contextual_poll_resolution(&ticket).unwrap(),
            ScopedObservationContextualPollResolution::Cancelled
        ));
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
    async fn scoped_async_poll_driver_wakes_and_requeues_dropped_passes() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("async-poll-driver-root")),
        )
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

        let waiting_handle = handle.clone();
        let waiting_driver = tokio::spawn(async move { waiting_handle.next_poll_pass().await });
        tokio::task::yield_now().await;
        assert!(!waiting_driver.is_finished());

        let first_ticket = handle.host().request_poll().unwrap();
        let first_lease = tokio::time::timeout(std::time::Duration::from_secs(2), waiting_driver)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first_lease.target_generation(), 1);

        let second_ticket = handle.host().request_poll().unwrap();
        let followup_handle = handle.clone();
        let followup_driver = tokio::spawn(async move { followup_handle.next_poll_pass().await });
        tokio::task::yield_now().await;
        assert!(!followup_driver.is_finished());

        // Dropping the unfinished pass releases native-access serialization
        // before waking the next driver, so the combined target is immediately
        // reservable rather than transiently failing as "pass active".
        drop(first_lease);
        let followup_lease =
            tokio::time::timeout(std::time::Duration::from_secs(2), followup_driver)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .unwrap();
        assert_eq!(followup_lease.target_generation(), 2);
        drop(followup_lease);

        let barrier = handle.request_close();
        assert_eq!(
            first_ticket.wait_async().await,
            ScopedObservationPollResolution::Cancelled
        );
        assert_eq!(
            second_ticket.wait_async().await,
            ScopedObservationPollResolution::Cancelled
        );
        assert!(barrier.wait_async().await.complete);
    }

    #[tokio::test]
    async fn scoped_async_poll_driver_wakes_on_close_and_terminal_failure() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let closed_host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("async-poll-driver-close-root")),
        )
        .unwrap();
        let closed_runtime = ScopedObservationAsyncRuntime::open(
            closed_host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let closed_handle = closed_runtime.handle();
        let waiting_handle = closed_handle.clone();
        let closed_driver = tokio::spawn(async move { waiting_handle.next_poll_pass().await });
        tokio::task::yield_now().await;
        let closed_barrier = closed_handle.request_close();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), closed_driver)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(closed_barrier.wait_async().await.complete);

        let failed_host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("async-poll-driver-failure-root")),
        )
        .unwrap();
        let failed_runtime = ScopedObservationAsyncRuntime::open(
            failed_host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let failed_handle = failed_runtime.handle();
        let waiting_handle = failed_handle.clone();
        let failed_driver = tokio::spawn(async move { waiting_handle.next_poll_pass().await });
        tokio::task::yield_now().await;
        failed_handle
            .fail_observer(ScopedObserverFailureReason::InternalControlFailure, 777)
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), failed_driver)
                .await
                .unwrap()
                .unwrap(),
            Err(ScopedObservationPollError::ObserverFailed)
        ));
        assert!(failed_runtime.close().await.complete);
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
    async fn scoped_async_runtime_delivers_terminal_failure_before_bootstrap() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("async-runtime-failure-root")),
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
        let pending_poll = handle.host().request_poll().unwrap();
        let pending_ready = handle.host().ready_waiter().unwrap();
        let failure = handle
            .fail_observer(ScopedObserverFailureReason::NativeWatcherRoutingFailed, 17)
            .unwrap();
        assert_eq!(failure.failed_scope_epoch, 1);
        assert_eq!(failure.control_sequence, 1);
        assert_eq!(failure.phase, ScopedAppendDeliveryPhase::Bootstrap);
        assert_eq!(
            handle.with_attachment(|_, drain| drain.delivery_lane().state().continuity),
            ScopedObservationContinuity::Failed
        );
        assert!(matches!(
            pending_poll.wait_async().await,
            ScopedObservationPollResolution::Failed(failed)
                if Arc::ptr_eq(&failed, &failure)
        ));
        assert!(matches!(
            handle
                .host()
                .contextual_poll_resolution(&pending_poll)
                .unwrap(),
            ScopedObservationContextualPollResolution::Failed(failed)
                if Arc::ptr_eq(&failed, &failure)
        ));
        assert!(matches!(
            pending_ready.wait_async().await,
            ScopedObservationReadyResolution::Failed(failed)
                if Arc::ptr_eq(&failed, &failure)
        ));
        assert!(handle.host().poll_state().failed);
        assert!(matches!(
            handle.host().request_poll(),
            Err(ScopedObservationPollError::ObserverFailed)
        ));

        let repeated = handle
            .fail_observer(ScopedObserverFailureReason::InternalControlFailure, 99)
            .unwrap();
        assert!(Arc::ptr_eq(&failure, &repeated));
        let yielded = runtime.next_event().await.unwrap().unwrap();
        assert_eq!(yielded.envelope.observer_sequence, 1);
        assert_eq!(yielded.envelope.observed_at, 17);
        assert!(matches!(
            &yielded.envelope.event,
            ScopedObservationEvent::ObserverFailed {
                failure: delivered,
            } if Arc::ptr_eq(delivered, &failure)
        ));
        let applied = runtime
            .acknowledge_applied(yielded.application_receipt())
            .unwrap();
        assert_eq!(applied.applied_through_sequence, 1);
        assert!(runtime.close().await.complete);
    }

    #[tokio::test]
    async fn scoped_async_source_owner_drives_poll_and_waits_for_delivery_capacity() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("async-runtime-bootstrap-root");
        std::fs::create_dir_all(&root).unwrap();
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

        let active = handle
            .with_attachment(|host, drain| {
                host.bind_consumer_bootstrap_epoch_state(vec![object], admission, projection, drain)
            })
            .unwrap();
        let wrong_binding = ScopedObservationAppendPassBinding::new(
            "root-object",
            vec![ScopedObservationOwnedIdentityInput::new(
                "native-session-id",
                b"wrong-session".to_vec(),
            )
            .unwrap()],
            None,
            1,
            64,
            origin.clone(),
            false,
        )
        .unwrap();
        let failed_binding = handle
            .bind_epoch_source_owner(
                active,
                vec![wrong_binding],
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .unwrap_err();
        assert!(matches!(
            failed_binding.error(),
            ScopedObservationSourceOwnerBindingError::AccessIdentityMismatch
        ));
        let failed_debug = format!("{failed_binding:?}");
        assert!(failed_debug.contains("native-session-id"));
        assert!(!failed_debug.contains("wrong-session"));
        assert!(!failed_debug.contains("async-runtime-session"));
        let (_, active, _, directory_bindings) = failed_binding.into_parts();
        assert!(directory_bindings.is_empty());

        let mut wrong_origin = origin.clone();
        wrong_origin.object_id = 999;
        let wrong_origin_binding = ScopedObservationAppendPassBinding::new(
            "root-object",
            vec![ScopedObservationOwnedIdentityInput::new(
                "native-session-id",
                b"async-runtime-session".to_vec(),
            )
            .unwrap()],
            None,
            1,
            64,
            wrong_origin,
            false,
        )
        .unwrap();
        let failed_origin = handle
            .bind_epoch_source_owner(
                active,
                vec![wrong_origin_binding],
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .unwrap_err();
        assert_eq!(
            failed_origin.error(),
            ScopedObservationSourceOwnerBindingError::AccessIdentityMismatch
        );
        let (_, active, _, directory_bindings) = failed_origin.into_parts();
        assert!(directory_bindings.is_empty());

        let binding = ScopedObservationAppendPassBinding::new(
            "root-object",
            vec![ScopedObservationOwnedIdentityInput::new(
                "native-session-id",
                b"async-runtime-session".to_vec(),
            )
            .unwrap()],
            None,
            1,
            64,
            origin.clone(),
            false,
        )
        .unwrap();
        let binding_debug = format!("{binding:?}");
        assert!(binding_debug.contains("native-session-id"));
        assert!(!binding_debug.contains("async-runtime-session"));
        let owner = handle
            .bind_epoch_source_owner(
                active,
                vec![binding],
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .unwrap();
        assert_eq!(owner.scope_epoch(), 1);
        assert!(!format!("{owner:?}").contains("async-runtime-session"));
        let attempts = Arc::new(AtomicUsize::new(0));
        let source_attempts = Arc::clone(&attempts);
        let source_task = tokio::spawn(owner.run_until_stopped_with_clock(move || {
            100 + source_attempts.fetch_add(1, Ordering::SeqCst) as i64
        }));

        // The automatic owner reserves and completes the first live pass.
        // Leave its created-source control queued to force bounded pressure on
        // the immediately following deletion pass.
        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        let created_poll =
            tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll_contextual())
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            created_poll,
            ScopedObservationContextualPollResolution::Ready(_)
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        std::fs::remove_file(root.join("session.jsonl")).unwrap();
        let deletion_handle = handle.clone();
        let deletion_poll = tokio::spawn(async move { deletion_handle.poll().await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while attempts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let blocked_attempts = attempts.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), blocked_attempts);
        assert!(!deletion_poll.is_finished());

        // Dequeue is the exact capacity release. It wakes the owner, which
        // first flushes the already-admitted deletion and only then completes
        // the fresh exact-scope retry watermark.
        let created = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            created.envelope.event,
            ScopedObservationEvent::SourcePresence {
                change: ScopedAppendPresenceChange::Created { generation: 1 }
            }
        ));
        runtime
            .acknowledge_applied(created.application_receipt())
            .unwrap();
        let deletion_resolution =
            tokio::time::timeout(std::time::Duration::from_secs(2), deletion_poll)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(matches!(
            deletion_resolution,
            ScopedObservationPollResolution::Ready(_)
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), blocked_attempts + 1);
        let deleted = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            deleted.envelope.event,
            ScopedObservationEvent::SourcePresence {
                change: ScopedAppendPresenceChange::Deleted { generation: 1 }
            }
        ));
        runtime
            .acknowledge_applied(deleted.application_receipt())
            .unwrap();

        // A continuity control must wake a source owner parked with no poll or
        // retry work. The returned handoff retains the exact old epoch,
        // redacted bindings, and retry policy needed to build the replacement
        // and bind its new automatic owner.
        assert!(!source_task.is_finished());
        let attempts_before_resync = attempts.load(Ordering::SeqCst);
        let required = handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 200)
            .unwrap();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), source_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(invalidation) =
            stopped.exit()
        else {
            panic!("resync must stop the exact old-epoch source owner");
        };
        assert_eq!(invalidation.owned_scope_epoch(), 1);
        assert_eq!(invalidation.observed_scope_epoch(), 1);
        assert_eq!(
            invalidation.observed_continuity(),
            ScopedObservationContinuity::ResyncRequired
        );
        assert!(Arc::ptr_eq(&invalidation.control().unwrap(), &required));
        assert_eq!(stopped.binding_count(), 1);
        assert_eq!(
            stopped.retry_policy(),
            ScopedObservationSourceOwnerRetryPolicy::default()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), attempts_before_resync);
        let (mut active, bindings, directory_bindings, policy, exit) = stopped.into_rebind_parts();
        assert!(directory_bindings.is_empty());
        assert!(matches!(
            exit,
            ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(_)
        ));
        assert_eq!(bindings.len(), 1);
        assert!(!active
            .append_object("root-object")
            .expect("the exact root remains bound after deletion")
            .is_present());

        // The invalid old epoch cannot be rebound before replacement, and a
        // failed bind still returns the same handoff material intact.
        let failed_rebind = handle
            .bind_epoch_source_owner(active, bindings, policy)
            .unwrap_err();
        assert_eq!(
            failed_rebind.error(),
            ScopedObservationSourceOwnerBindingError::InvalidEpochState
        );
        let (_, returned_active, returned_bindings, directory_bindings) =
            failed_rebind.into_parts();
        assert!(directory_bindings.is_empty());
        active = returned_active;
        assert_eq!(returned_bindings.len(), 1);

        let required_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &required_event.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { control }
                if Arc::ptr_eq(control, &required)
        ));
        runtime
            .acknowledge_applied(required_event.application_receipt())
            .unwrap();
        let started = handle.begin_resync(210).unwrap();
        assert_eq!(started.old_scope_epoch, 1);
        assert_eq!(started.new_scope_epoch, 2);
        let started_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &started_event.envelope.event,
            ScopedObservationEvent::ObserverResyncStarted { control }
                if Arc::ptr_eq(control, &started)
        ));
        runtime
            .acknowledge_applied(started_event.application_receipt())
            .unwrap();

        let mut stage = handle.open_scope_resync_stage(&mut active).unwrap();
        assert_eq!(stage.scope_epoch(), 2);
        std::fs::write(root.join("session.jsonl"), b"replacement\n").unwrap();
        let replacement_pass = handle.host().begin_pass().unwrap();
        let replacement_observation = handle.with_attachment(|_, drain| {
            let (object, _) = stage
                .append_object_and_admission_mut("root-object", drain.delivery_lane())
                .unwrap();
            object
                .reconcile(
                    &replacement_pass,
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
                .unwrap()
        });
        assert_eq!(
            replacement_observation.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        let replacement_decoded = handle.with_attachment(|host, drain| {
            let (object, _) = stage
                .append_object_and_admission_mut("root-object", drain.delivery_lane())
                .unwrap();
            let ScopedAppendDecodeOutcome::Ready(decoded) =
                decode_scoped(host, object, &replacement_observation)
            else {
                panic!("replacement source must decode one bounded batch");
            };
            decoded
        });
        handle.with_attachment(|_, drain| {
            let (object, admission) = stage
                .append_object_and_admission_mut("root-object", drain.delivery_lane())
                .unwrap();
            if let Err(failure) =
                admission.admit(object, &replacement_observation, replacement_decoded)
            {
                panic!("replacement admission failed: {}", failure.error);
            }
        });
        drop(replacement_pass);
        handle.with_attachment(|_, drain| {
            assert!(stage.reduce_next(drain.delivery_lane()).unwrap());
            assert!(!stage.reduce_next(drain.delivery_lane()).unwrap());
            let replacement = stage.prepare_snapshot(drain.delivery_lane()).unwrap();
            assert_eq!(replacement.entity_count, 0);
            assert!(replacement.events.is_empty());
            assert!(stage.snapshot_fully_offered());
        });
        let completed = handle
            .offer_scope_resync_complete(&mut active, &mut stage, 220)
            .unwrap();
        assert_eq!(active.scope_epoch(), 2);
        let completed_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &completed_event.envelope.event,
            ScopedObservationEvent::ObserverResyncComplete { barrier }
                if Arc::ptr_eq(barrier, &completed)
        ));
        runtime
            .acknowledge_applied(completed_event.application_receipt())
            .unwrap();

        let replacement_owner = handle
            .bind_epoch_source_owner(active, returned_bindings, policy)
            .unwrap();
        assert_eq!(replacement_owner.scope_epoch(), 2);
        let replacement_task = tokio::spawn(replacement_owner.run_until_stopped_with_clock(|| 230));
        let replacement_poll =
            tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
                .await
                .unwrap()
                .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = replacement_poll else {
            panic!("the replacement source owner must service the new epoch");
        };
        assert_eq!(watermark.scope_epoch, 2);

        let close = runtime.request_close();
        let replacement_stopped =
            tokio::time::timeout(std::time::Duration::from_secs(2), replacement_task)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            replacement_stopped.exit(),
            ScopedObservationSourceOwnerRunExit::Cancelled
        ));
        assert!(close.wait_async().await.complete);
        assert!(runtime.next_event().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn scoped_applied_ready_tracks_consumer_ack_failure_and_close() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();

        let (applied_runtime, applied_handle, applied_pair, _target, applied_drops) =
            automatic_single_object_pair(
                &registry,
                temp.path().join("applied-ready-complete-root"),
                b"applied-ready-complete-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .await;
        let expected = applied_handle
            .with_attachment(|_, drain| drain.consumer_bootstrap_barrier())
            .unwrap();
        assert!(matches!(
            applied_handle.ready_applied().await.unwrap(),
            ScopedObservationReadyResolution::Ready(barrier)
                if Arc::ptr_eq(&barrier, &expected)
        ));
        applied_handle
            .fail_observer(ScopedObserverFailureReason::InternalControlFailure, 9)
            .unwrap();
        assert!(matches!(
            applied_handle.ready_applied().await.unwrap(),
            ScopedObservationReadyResolution::Ready(barrier)
                if Arc::ptr_eq(&barrier, &expected)
        ));
        drop(applied_pair);
        assert!(applied_runtime.close().await.complete);
        assert_eq!(applied_drops.load(Ordering::SeqCst), 1);

        let failed_host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("applied-ready-failed-root")),
        )
        .unwrap();
        let failed_runtime = ScopedObservationAsyncRuntime::open(
            failed_host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let failed_handle = failed_runtime.handle();
        let failed_wait = failed_handle.ready_applied();
        tokio::pin!(failed_wait);
        tokio::select! {
            biased;
            resolution = &mut failed_wait => {
                panic!("consumer readiness resolved before application or failure: {resolution:?}");
            }
            _ = tokio::task::yield_now() => {}
        }
        let failure = failed_handle
            .fail_observer(ScopedObserverFailureReason::InternalControlFailure, 10)
            .unwrap();
        assert!(matches!(
            failed_wait.await.unwrap(),
            ScopedObservationReadyResolution::Failed(resolved)
                if Arc::ptr_eq(&resolved, &failure)
        ));
        assert!(failed_runtime.close().await.complete);

        let closed_host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("applied-ready-closed-root")),
        )
        .unwrap();
        let closed_runtime = ScopedObservationAsyncRuntime::open(
            closed_host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let closed_handle = closed_runtime.handle();
        let cancelled_wait = closed_handle.ready_applied();
        tokio::pin!(cancelled_wait);
        tokio::select! {
            biased;
            resolution = &mut cancelled_wait => {
                panic!("consumer readiness resolved before application or close: {resolution:?}");
            }
            _ = tokio::task::yield_now() => {}
        }
        let close = closed_runtime.request_close();
        assert!(matches!(
            cancelled_wait.await.unwrap(),
            ScopedObservationReadyResolution::Cancelled
        ));
        assert!(close.wait_async().await.complete);
    }

    #[tokio::test]
    async fn scoped_explicit_resync_rejects_bootstrap_and_distinguishes_terminal_outcomes() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();

        let bootstrap_host = ScopedObservationAccessHost::authorize(
            &registry,
            scoped_access_request(temp.path().join("explicit-resync-bootstrap-root")),
        )
        .unwrap();
        let bootstrap_runtime = ScopedObservationAsyncRuntime::open(
            bootstrap_host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 1,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            },
        )
        .unwrap();
        let bootstrap_handle = bootstrap_runtime.handle();
        assert_eq!(
            bootstrap_handle.resync_at(1).await.unwrap_err(),
            ScopedContinuityError::BootstrapIncomplete
        );
        let bootstrap_state =
            bootstrap_handle.with_attachment(|_, drain| drain.delivery_lane().state());
        assert_eq!(
            bootstrap_state.continuity,
            ScopedObservationContinuity::Bootstrap
        );
        assert_eq!(bootstrap_state.offered_through_sequence, 0);
        assert!(bootstrap_runtime.close().await.complete);

        let (failed_runtime, failed_handle, failed_pair, _target, failed_drops) =
            automatic_single_object_pair(
                &registry,
                temp.path().join("explicit-resync-failed-root"),
                b"explicit-resync-failed-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .await;
        let waiting_handle = failed_handle.clone();
        let failed_wait = tokio::spawn(async move { waiting_handle.resync_at(10).await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if failed_handle
                    .with_attachment(|_, drain| drain.delivery_lane().state().continuity)
                    == ScopedObservationContinuity::ResyncRequired
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let applied_waiting_handle = failed_handle.clone();
        let failed_applied =
            tokio::spawn(
                async move { applied_waiting_handle.resync_applied_at(10).await.unwrap() },
            );
        tokio::task::yield_now().await;
        let failure = failed_handle
            .fail_observer(ScopedObserverFailureReason::InternalControlFailure, 11)
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), failed_wait)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationResyncResolution::Failed(resolved)
                if Arc::ptr_eq(&resolved, &failure)
        ));
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), failed_applied)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationResyncResolution::Failed(resolved)
                if Arc::ptr_eq(&resolved, &failure)
        ));
        drop(failed_pair);
        assert!(failed_runtime.close().await.complete);
        assert_eq!(failed_drops.load(Ordering::SeqCst), 1);

        let (closed_runtime, closed_handle, closed_pair, _target, closed_drops) =
            automatic_single_object_pair(
                &registry,
                temp.path().join("explicit-resync-closed-root"),
                b"explicit-resync-closed-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .await;
        let waiting_handle = closed_handle.clone();
        let cancelled_wait =
            tokio::spawn(async move { waiting_handle.resync_at(20).await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if closed_handle
                    .with_attachment(|_, drain| drain.delivery_lane().state().continuity)
                    == ScopedObservationContinuity::ResyncRequired
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let applied_waiting_handle = closed_handle.clone();
        let cancelled_applied =
            tokio::spawn(
                async move { applied_waiting_handle.resync_applied_at(20).await.unwrap() },
            );
        tokio::task::yield_now().await;
        let close = closed_runtime.request_close();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), cancelled_wait)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationResyncResolution::Cancelled
        ));
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), cancelled_applied)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationResyncResolution::Cancelled
        ));
        drop(closed_pair);
        assert!(close.wait_async().await.complete);
        assert_eq!(closed_drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_explicit_resync_coalesces_at_engine_barrier_before_application() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("explicit-resync-coalescing-root");
        let (mut runtime, handle, pair, _target, drops) = automatic_single_object_pair(
            &registry,
            root,
            b"explicit-resync-coalescing-session".to_vec(),
            AppendDelimitedConfig::json_lines(),
            128,
            ScopedObservationSourceOwnerRetryPolicy::default(),
        )
        .await;

        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));
        let first_handle = handle.clone();
        let first = tokio::spawn(async move { first_handle.resync_at(70).await.unwrap() });
        let second_handle = handle.clone();
        let second = tokio::spawn(async move { second_handle.resync_at(71).await.unwrap() });
        let applied_handle = handle.clone();
        let applied =
            tokio::spawn(async move { applied_handle.resync_applied_at(73).await.unwrap() });

        let first_result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = first_result else {
            panic!("the explicit invalidation must retain the owner pair for replacement");
        };
        assert!(matches!(
            handoff.source().exit(),
            ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(invalidation)
                if invalidation.control().unwrap().reason
                    == ScopedResyncReason::ExplicitConsumerRequest
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            || 80,
            || 90,
        ));
        let required_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncRequired { control: required } =
            &required_event.envelope.event
        else {
            panic!("explicit resync must first deliver its required control");
        };
        assert_eq!(required.reason, ScopedResyncReason::ExplicitConsumerRequest);
        assert_eq!(required.invalid_scope_epoch, 1);
        runtime
            .acknowledge_applied(required_event.application_receipt())
            .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let state = handle.with_attachment(|_, drain| drain.delivery_lane().state());
                if state.continuity == ScopedObservationContinuity::Resyncing {
                    assert_eq!(state.scope_epoch, 2);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        // This caller begins only after the replacement has started. Holding
        // the started envelope in the drain prevents completion until the
        // task has observed and joined that exact lineage.
        let third_handle = handle.clone();
        let third = tokio::spawn(async move { third_handle.resync_at(72).await.unwrap() });
        tokio::task::yield_now().await;
        assert!(!third.is_finished());
        assert_eq!(
            handle.with_attachment(|_, drain| drain.delivery_lane().state().continuity),
            ScopedObservationContinuity::Resyncing
        );

        let started_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncStarted { control: started } =
            &started_event.envelope.event
        else {
            panic!("the joined replacement must deliver one started control");
        };
        assert_eq!(started.old_scope_epoch, 1);
        assert_eq!(started.new_scope_epoch, 2);
        assert_eq!(started.required_control_sequence, required.control_sequence);
        runtime
            .acknowledge_applied(started_event.application_receipt())
            .unwrap();

        let completed_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } =
            &completed_event.envelope.event
        else {
            panic!("the explicit replacement must offer one completion barrier");
        };
        assert_eq!(barrier.scope_epoch, 2);
        assert_eq!(runtime.applied_state().resync_barrier_sequence, None);
        assert!(handle
            .with_attachment(|_, drain| drain.consumer_resync_barrier())
            .is_none());
        assert!(Arc::ptr_eq(
            &handle
                .with_attachment(|_, drain| drain.engine_resync_barrier())
                .unwrap(),
            barrier
        ));

        for resolution in [first, second, third] {
            let resolution = tokio::time::timeout(std::time::Duration::from_secs(2), resolution)
                .await
                .unwrap()
                .unwrap();
            let ScopedObservationResyncResolution::Ready(resolved) = resolution else {
                panic!("every coalesced caller must resolve from the offered barrier");
            };
            assert!(Arc::ptr_eq(&resolved, barrier));
        }
        assert_eq!(runtime.applied_state().resync_barrier_sequence, None);
        assert!(!applied.is_finished());

        runtime
            .acknowledge_applied(completed_event.application_receipt())
            .unwrap();
        assert_eq!(
            runtime.applied_state().resync_barrier_sequence,
            Some(barrier.barrier_sequence)
        );
        assert!(Arc::ptr_eq(
            &handle
                .with_attachment(|_, drain| drain.consumer_resync_barrier())
                .unwrap(),
            barrier
        ));
        let ScopedObservationResyncResolution::Ready(applied_barrier) =
            tokio::time::timeout(std::time::Duration::from_secs(2), applied)
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("applied resync must resolve after the completion receipt");
        };
        assert!(Arc::ptr_eq(&applied_barrier, barrier));

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        // The retained epoch-2 barrier cannot satisfy a new request whose
        // invalid epoch is already 2.
        let next_handle = handle.clone();
        let next_applied =
            tokio::spawn(async move { next_handle.resync_applied_at(100).await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if handle.with_attachment(|_, drain| drain.delivery_lane().state().continuity)
                    == ScopedObservationContinuity::ResyncRequired
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!next_applied.is_finished());
        let close = runtime.request_close();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), next_applied)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationResyncResolution::Cancelled
        ));
        drop(pair);
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_three_observers_isolate_slow_overflow_from_healthy_progress() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let watcher_policy = || {
            ScopedObservationNativeWatcherRecoveryPolicy::new(
                std::time::Duration::from_secs(60),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                1,
            )
            .unwrap()
        };
        let source_policy = ScopedObservationSourceOwnerRetryPolicy::default();

        let (slow_runtime, slow_handle, slow_pair, slow_target, slow_drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("multi-observer-slow-root"),
                    b"multi-observer-slow-session".to_vec(),
                ),
                b"multi-observer-slow-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy()),
            )
            .await;
        let (mut first_runtime, first_handle, first_pair, first_target, first_drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("multi-observer-first-root"),
                    b"multi-observer-first-session".to_vec(),
                ),
                b"multi-observer-first-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy()),
            )
            .await;
        let (mut second_runtime, second_handle, second_pair, second_target, second_drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("multi-observer-second-root"),
                    b"multi-observer-second-session".to_vec(),
                ),
                b"multi-observer-second-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy()),
            )
            .await;

        let roots = [
            &slow_handle.host().root_identity().session_key,
            &first_handle.host().root_identity().session_key,
            &second_handle.host().root_identity().session_key,
        ];
        assert_ne!(roots[0], roots[1]);
        assert_ne!(roots[0], roots[2]);
        assert_ne!(roots[1], roots[2]);

        let slow_task =
            tokio::spawn(slow_pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let first_task =
            tokio::spawn(first_pair.run_with_factory_and_clocks(|_| Err(()), || 200, || 201));
        let second_task =
            tokio::spawn(second_pair.run_with_factory_and_clocks(|_| Err(()), || 300, || 301));

        // Do not drain the slow observer's created-source control. Its exact
        // attachment becomes continuity-invalid while both sibling runtimes
        // retain independent queue, poll, source-owner, and watcher state.
        std::fs::write(&slow_target, b"slow\n").unwrap();
        let slow_poll = tokio::time::timeout(std::time::Duration::from_secs(2), slow_handle.poll())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            slow_poll,
            ScopedObservationPollResolution::Ready(_)
        ));
        assert_eq!(
            slow_handle
                .with_attachment(|_, drain| drain.delivery_lane().queued_source_control_items()),
            1
        );
        let slow_required = slow_handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 110)
            .unwrap();
        let slow_result = tokio::time::timeout(std::time::Duration::from_secs(2), slow_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(slow_handoff) = slow_result else {
            panic!("only the slow observer must retain a replacement handoff");
        };
        assert!(matches!(
            slow_handoff.source().exit(),
            ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(invalidation)
                if Arc::ptr_eq(&invalidation.control().unwrap(), &slow_required)
        ));

        std::fs::write(&first_target, b"first\n").unwrap();
        std::fs::write(&second_target, b"second\n").unwrap();
        let (first_poll, second_poll) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(first_handle.poll(), second_handle.poll())
            })
            .await
            .unwrap();
        let ScopedObservationPollResolution::Ready(first_watermark) = first_poll.unwrap() else {
            panic!("the first healthy observer must complete its own poll");
        };
        let ScopedObservationPollResolution::Ready(second_watermark) = second_poll.unwrap() else {
            panic!("the second healthy observer must complete its own poll");
        };
        assert_eq!(first_watermark.scope_epoch, 1);
        assert_eq!(second_watermark.scope_epoch, 1);
        assert_eq!(&first_watermark.root, first_handle.host().root_identity());
        assert_eq!(&second_watermark.root, second_handle.host().root_identity());
        assert_eq!(
            first_handle.with_attachment(|_, drain| drain.delivery_lane().state().continuity),
            ScopedObservationContinuity::Valid
        );
        assert_eq!(
            second_handle.with_attachment(|_, drain| drain.delivery_lane().state().continuity),
            ScopedObservationContinuity::Valid
        );

        let (first_event, second_event) =
            tokio::join!(first_runtime.next_event(), second_runtime.next_event());
        let first_event = first_event.unwrap().unwrap();
        let second_event = second_event.unwrap().unwrap();
        assert!(matches!(
            first_event.envelope.event,
            ScopedObservationEvent::SourcePresence {
                change: ScopedAppendPresenceChange::Created { generation: 1 }
            }
        ));
        assert!(matches!(
            second_event.envelope.event,
            ScopedObservationEvent::SourcePresence {
                change: ScopedAppendPresenceChange::Created { generation: 1 }
            }
        ));
        assert_eq!(first_event.envelope.observer_sequence, 2);
        assert_eq!(second_event.envelope.observer_sequence, 2);
        first_runtime
            .acknowledge_applied(first_event.application_receipt())
            .unwrap();
        second_runtime
            .acknowledge_applied(second_event.application_receipt())
            .unwrap();
        assert_eq!(slow_drops.load(Ordering::SeqCst), 0);
        assert_eq!(first_drops.load(Ordering::SeqCst), 0);
        assert_eq!(second_drops.load(Ordering::SeqCst), 0);

        let first_close = first_runtime.request_close();
        let second_close = second_runtime.request_close();
        let (first_stopped, second_stopped) = tokio::join!(first_task, second_task);
        assert!(matches!(
            first_stopped.unwrap(),
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(matches!(
            second_stopped.unwrap(),
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(first_close.wait_async().await.complete);
        assert!(second_close.wait_async().await.complete);

        let slow_close = slow_runtime.request_close();
        drop(slow_handoff);
        assert!(slow_close.wait_async().await.complete);
        assert_eq!(slow_drops.load(Ordering::SeqCst), 1);
        assert_eq!(first_drops.load(Ordering::SeqCst), 1);
        assert_eq!(second_drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_busy_observer_yields_bounded_passes_before_sibling_starvation() {
        const BUSY_PASS_LIMIT: usize = 128;

        assert!(SharedSourcePassPool::new(0).is_err());
        assert!(SharedSourcePassPool::new(usize::MAX).is_err());
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let watcher_policy = || {
            ScopedObservationNativeWatcherRecoveryPolicy::new(
                std::time::Duration::from_secs(60),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                1,
            )
            .unwrap()
        };
        let source_policy = ScopedObservationSourceOwnerRetryPolicy::default();
        let pass_pool = SharedSourcePassPool::new(1).unwrap();
        assert_eq!(pass_pool.max_concurrent_passes(), 1);
        let (busy_runtime, busy_handle, busy_pair, _, busy_drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("busy-observer-root"),
                    b"busy-observer-session".to_vec(),
                ),
                b"busy-observer-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy())
                    .with_pass_pool(pass_pool.clone()),
            )
            .await;
        let (healthy_runtime, healthy_handle, healthy_pair, _, healthy_drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("healthy-observer-root"),
                    b"healthy-observer-session".to_vec(),
                ),
                b"healthy-observer-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy())
                    .with_pass_pool(pass_pool.clone()),
            )
            .await;

        let busy_seed = busy_handle.host().request_poll().unwrap();
        let healthy_poll = healthy_handle.host().request_poll().unwrap();
        let busy_passes = Arc::new(AtomicUsize::new(0));
        let keep_busy = Arc::new(AtomicBool::new(true));
        let busy_clock_handle = busy_handle.clone();
        let busy_clock_passes = Arc::clone(&busy_passes);
        let busy_clock_enabled = Arc::clone(&keep_busy);
        let busy_task = tokio::spawn(busy_pair.run_with_factory_and_clocks(
            |_| Err(()),
            || 100,
            move || {
                let pass = busy_clock_passes.fetch_add(1, Ordering::SeqCst) + 1;
                if busy_clock_enabled.load(Ordering::SeqCst) && pass < BUSY_PASS_LIMIT {
                    // This request is admitted while the current lease is
                    // active, keeping a follow-up pass continuously runnable.
                    let _ = busy_clock_handle.host().request_poll().unwrap();
                }
                100 + i64::try_from(pass).unwrap()
            },
        ));
        let healthy_task =
            tokio::spawn(healthy_pair.run_with_factory_and_clocks(|_| Err(()), || 200, || 201));

        let healthy_watermark =
            tokio::time::timeout(std::time::Duration::from_secs(2), healthy_poll.wait_async())
                .await
                .unwrap();
        assert!(matches!(
            healthy_watermark,
            ScopedObservationPollResolution::Ready(_)
        ));
        assert!(
            busy_passes.load(Ordering::SeqCst) < BUSY_PASS_LIMIT,
            "the healthy observer must run before one busy scope exhausts its continuously runnable pass chain"
        );
        keep_busy.store(false, Ordering::SeqCst);
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), busy_seed.wait_async())
                .await
                .unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));

        let busy_close = busy_runtime.request_close();
        let healthy_close = healthy_runtime.request_close();
        let (busy_stopped, healthy_stopped) = tokio::join!(busy_task, healthy_task);
        assert!(matches!(
            busy_stopped.unwrap(),
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(matches!(
            healthy_stopped.unwrap(),
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(busy_close.wait_async().await.complete);
        assert!(healthy_close.wait_async().await.complete);
        assert_eq!(busy_drops.load(Ordering::SeqCst), 1);
        assert_eq!(healthy_drops.load(Ordering::SeqCst), 1);
        assert_eq!(pass_pool.available_permits(), 1);
    }

    #[tokio::test]
    async fn scoped_pass_pool_wait_is_close_cancellable_without_native_access() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let pass_pool = SharedSourcePassPool::new(1).unwrap();
        let held_permit = pass_pool.acquire_for_test().await;
        let (runtime, handle, pair, _, drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("permit-wait-close-root"),
                    b"permit-wait-close-session".to_vec(),
                ),
                b"permit-wait-close-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(
                    ScopedObservationSourceOwnerRetryPolicy::default(),
                    ScopedObservationNativeWatcherRecoveryPolicy::new(
                        std::time::Duration::from_secs(60),
                        std::time::Duration::from_millis(1),
                        std::time::Duration::from_millis(1),
                        1,
                    )
                    .unwrap(),
                )
                .with_pass_pool(pass_pool.clone()),
            )
            .await;
        let ticket = handle.host().request_poll().unwrap();
        let owner = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        tokio::task::yield_now().await;
        assert_eq!(pass_pool.available_permits(), 0);
        assert_eq!(
            handle.host().poll_resolution(&ticket).unwrap(),
            ScopedObservationPollResolution::Pending
        );

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert_eq!(ticket.wait(), ScopedObservationPollResolution::Cancelled);
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(pass_pool.available_permits(), 0);
        drop(held_permit);
        assert_eq!(pass_pool.available_permits(), 1);
    }

    #[tokio::test]
    async fn scoped_async_owner_pair_retains_watcher_across_resync_and_rebinds_epoch() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("async-owner-pair-root");
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
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let watcher = handle
            .install_native_watcher_with_factory(2, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();

        let mut append_config = AppendDelimitedConfig::json_lines();
        append_config.max_record_bytes = 16;
        append_config.max_batch_bytes = 20;
        append_config.max_records_per_batch = 2;
        append_config.prefix_anchor_bytes = 8;
        let mut object = scoped_append_object_with_coverage(
            AppendDelimitedFile::new(append_config).unwrap(),
            RawRetentionPolicy::None,
            vec![CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            }],
        );
        let mut admission = admission_lane(2, 0, 1);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"async-owner-pair-session",
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
        let observation = reconcile_scoped_append(
            handle.host(),
            scan.access_pass(),
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Initial,
        );
        assert!(!observation.object_present);
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
        assert!(matches!(
            watcher
                .coordinator()
                .next_reconcile(handle.host(), 2)
                .unwrap(),
            ScopedObservationStartupReconcileAction::CaughtUp
        ));
        object.complete_bootstrap().unwrap();
        let bootstrap = handle
            .with_attachment(|host, drain| {
                watcher.coordinator().offer_bootstrap_complete(
                    host,
                    std::slice::from_ref(&object),
                    &admission,
                    &projection,
                    drain,
                    50,
                )
            })
            .unwrap();
        assert!(matches!(
            watcher.coordinator().phase(),
            ScopedObservationWatcherPhase::Live { .. }
        ));
        let bootstrap_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &bootstrap_event.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { barrier }
                if Arc::ptr_eq(barrier, &bootstrap)
        ));
        runtime
            .acknowledge_applied(bootstrap_event.application_receipt())
            .unwrap();

        let active = handle
            .with_attachment(|host, drain| {
                host.bind_consumer_bootstrap_epoch_state(vec![object], admission, projection, drain)
            })
            .unwrap();
        let binding = ScopedObservationAppendPassBinding::new(
            "root-object",
            vec![ScopedObservationOwnedIdentityInput::new(
                "native-session-id",
                b"async-owner-pair-session".to_vec(),
            )
            .unwrap()],
            None,
            1,
            64,
            origin.clone(),
            true,
        )
        .unwrap();
        let source_policy = ScopedObservationSourceOwnerRetryPolicy::default();
        let source = handle
            .bind_epoch_source_owner(active, vec![binding], source_policy)
            .unwrap();
        let watcher_policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            1,
        )
        .unwrap();
        let pair = handle
            .bind_live_owner_pair(watcher, source, watcher_policy)
            .unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));

        // The first replacement decode is genuinely transient. The invalid
        // epoch has no requested poll, so only the replacement owner may read
        // it and must retain the watcher while applying the source-owner's
        // bounded retry policy.
        std::fs::write(&target, b"retry\n").unwrap();
        let required = handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let first_result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = first_result else {
            panic!("continuity invalidation must preserve the watcher in a resync handoff");
        };
        assert!(matches!(
            handoff.source().exit(),
            ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(invalidation)
                if Arc::ptr_eq(&invalidation.control().unwrap(), &required)
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(callback_slot.lock().unwrap().is_some());
        assert_eq!(registrations.lock().unwrap().len(), 1);

        // Native callbacks remain owned and routable while no source owner is
        // allowed to read the invalid epoch. The automatic handoff must not
        // consume the application lane in order to make progress.
        callback_slot.lock().unwrap().as_mut().unwrap()(Ok(notify::Event::new(
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
        )
        .add_path(target.clone())));
        let pending_poll = handle.host().poll_state();
        assert!(
            pending_poll.requested_through_generation > pending_poll.completed_through_generation
        );

        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            || 80,
            || 90,
        ));
        let required_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &required_event.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { control }
                if Arc::ptr_eq(control, &required)
        ));
        runtime
            .acknowledge_applied(required_event.application_receipt())
            .unwrap();
        let started_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncStarted { control } =
            &started_event.envelope.event
        else {
            panic!(
                "automatic replacement must emit resync-started before replay: {:?}",
                started_event.envelope.event
            );
        };
        assert_eq!(control.old_scope_epoch, 1);
        assert_eq!(control.new_scope_epoch, 2);
        runtime
            .acknowledge_applied(started_event.application_receipt())
            .unwrap();
        let retry_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::SourceObjectError { error } = &retry_event.envelope.event
        else {
            panic!("replacement retry must publish its relation-local error");
        };
        assert_eq!(retry_event.envelope.scope_epoch, 2);
        assert_eq!(
            retry_event.envelope.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert_eq!(
            error.failure_code,
            ScopedSourceObjectFailureCode::DecodeRetryTransient
        );
        assert_eq!(
            error.retry,
            ScopedSourceObjectRetryState::RetryScheduled {
                failed_attempts: 1,
                max_attempts: 5,
                retry_after_ms: 100,
            }
        );
        assert!(error.provenance.last_successful_position.is_none());
        // Recover before the scheduled retry. The successful attempt must
        // clear the staged error, then drain all five records across multiple
        // bounded batches without carrying the transient into the barrier.
        std::fs::write(&target, b"rec-01\nrec-02\nrec-03\nrec-04\nrec-05\n").unwrap();
        runtime
            .acknowledge_applied(retry_event.application_receipt())
            .unwrap();
        let completed_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } =
            &completed_event.envelope.event
        else {
            panic!("automatic replacement must end with one completion barrier");
        };
        assert_eq!(barrier.scope_epoch, 2);
        assert!(barrier.root_present);
        assert!(barrier.explicit_object_errors.is_empty());
        assert!(!barrier.source_coverage.is_empty());
        assert!(barrier.source_coverage.iter().all(|coverage| {
            coverage.completeness == CoverageSetCompleteness::Complete
                && coverage.explicit_errors.is_empty()
        }));
        let replay_positions = barrier
            .source_coverage
            .iter()
            .flat_map(|coverage| &coverage.points)
            .filter_map(|point| point.position.as_ref())
            .collect::<Vec<_>>();
        assert!(!replay_positions.is_empty());
        assert!(replay_positions.iter().all(|position| {
            position.kind == CoveragePositionKind::AppendCursor
                && position.monotonic_order == Some(35)
        }));
        runtime
            .acknowledge_applied(completed_event.application_receipt())
            .unwrap();

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let resumed_task =
            tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let resolution = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = resolution else {
            panic!("the re-paired source must service the replacement epoch");
        };
        assert_eq!(watermark.scope_epoch, 2);
        assert!(watermark.explicit_object_errors.is_empty());
        assert!(watermark.source_coverage.iter().all(|coverage| {
            coverage.points.iter().all(|point| {
                point.generation == 1
                    && point.position.as_ref().is_some_and(|position| {
                        position.kind == CoveragePositionKind::AppendCursor
                            && position.monotonic_order == Some(35)
                    })
            })
        }));
        assert_eq!(registrations.lock().unwrap().len(), 1);
        let settled_poll = handle.host().poll_state();
        assert_eq!(
            settled_poll.completed_through_generation,
            settled_poll.requested_through_generation
        );
        assert!(
            settled_poll.completed_through_generation >= pending_poll.requested_through_generation
        );

        callback_slot.lock().unwrap().as_mut().unwrap()(Err(notify::Error::generic(
            "fixture paired watcher permanently unavailable",
        )));
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Stopped(stopped) = stopped else {
            panic!("permanent watcher failure must stop both supervised owners");
        };
        let failure = stopped
            .terminal_failure()
            .expect("permanent native failure must remain available for diagnostics");
        assert!(matches!(
            stopped.first_exit(),
            ScopedObservationAsyncOwnerFirstExit::Watcher(
                ScopedObservationNativeWatcherRunExit::Failed(delivered)
            ) if Arc::ptr_eq(delivered, &failure)
        ));
        assert!(matches!(
            stopped.source().exit(),
            ScopedObservationSourceOwnerRunExit::Failed(
                ScopedObservationSourceOwnerRunError::Pass(
                    ScopedObservationPassExecutionError::Poll(
                        ScopedObservationPollError::ObserverFailed
                    )
                )
            )
        ));
        assert_eq!(
            failure.reason,
            ScopedObserverFailureReason::NativeWatcherRecoveryExhausted
        );
        assert!(stopped.supervision_error().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!handle.host().is_closed());

        let failed_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &failed_event.envelope.event,
            ScopedObservationEvent::ObserverFailed { failure: delivered }
                if Arc::ptr_eq(delivered, &failure)
        ));
        runtime
            .acknowledge_applied(failed_event.application_receipt())
            .unwrap();
        assert!(runtime.close().await.complete);
        assert!(runtime.next_event().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn scoped_automatic_resync_publishes_first_access_terminal_error_without_old_state() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("automatic-resync-failure-root");
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
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let watcher = handle
            .install_native_watcher_with_factory(2, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
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
            value: b"automatic-resync-failure-session",
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
        let observation = reconcile_scoped_append(
            handle.host(),
            scan.access_pass(),
            &mut object,
            &mut admission,
            &identity,
            &origin,
            AccessPhase::Initial,
        );
        assert!(!observation.object_present);
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
        assert!(matches!(
            watcher
                .coordinator()
                .next_reconcile(handle.host(), 2)
                .unwrap(),
            ScopedObservationStartupReconcileAction::CaughtUp
        ));
        object.complete_bootstrap().unwrap();
        let bootstrap = handle
            .with_attachment(|host, drain| {
                watcher.coordinator().offer_bootstrap_complete(
                    host,
                    std::slice::from_ref(&object),
                    &admission,
                    &projection,
                    drain,
                    50,
                )
            })
            .unwrap();
        let bootstrap_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &bootstrap_event.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { barrier }
                if Arc::ptr_eq(barrier, &bootstrap)
        ));
        runtime
            .acknowledge_applied(bootstrap_event.application_receipt())
            .unwrap();

        let active = handle
            .with_attachment(|host, drain| {
                host.bind_consumer_bootstrap_epoch_state(vec![object], admission, projection, drain)
            })
            .unwrap();
        let binding = ScopedObservationAppendPassBinding::new(
            "root-object",
            vec![ScopedObservationOwnedIdentityInput::new(
                "native-session-id",
                b"automatic-resync-failure-session".to_vec(),
            )
            .unwrap()],
            None,
            1,
            64,
            origin,
            false,
        )
        .unwrap();
        let source = handle
            .bind_epoch_source_owner(
                active,
                vec![binding],
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .unwrap();
        let watcher_policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            1,
        )
        .unwrap();
        let pair = handle
            .bind_live_owner_pair(watcher, source, watcher_policy)
            .unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));

        // The declared per-access physical ceiling is 64 bytes. Because this
        // object fails before any replacement cursor or semantic state can
        // commit, the replacement may honestly remove the old epoch and make
        // the relation explicitly unavailable. It must not fail the watcher
        // or fabricate a partial cursor.
        std::fs::write(&target, vec![b'x'; 80]).unwrap();
        handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("continuity invalidation must yield a replacement handoff");
        };
        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            || 80,
            || 90,
        ));
        let required_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            required_event.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { .. }
        ));
        runtime
            .acknowledge_applied(required_event.application_receipt())
            .unwrap();

        let started_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            started_event.envelope.event,
            ScopedObservationEvent::ObserverResyncStarted { .. }
        ));
        runtime
            .acknowledge_applied(started_event.application_receipt())
            .unwrap();

        let object_error_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::SourceObjectError { error } =
            &object_error_event.envelope.event
        else {
            panic!(
                "terminal replacement source failure must be explicit: {:?}",
                object_error_event.envelope.event
            );
        };
        assert_eq!(object_error_event.envelope.scope_epoch, 2);
        assert_eq!(
            object_error_event.envelope.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert_eq!(
            error.failure_code,
            ScopedSourceObjectFailureCode::SourceLimitExceeded
        );
        assert_eq!(
            error.retry,
            ScopedSourceObjectRetryState::NotRetryable { failed_attempts: 1 }
        );
        assert!(error.provenance.last_successful_position.is_none());
        runtime
            .acknowledge_applied(object_error_event.application_receipt())
            .unwrap();

        let completed_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } =
            &completed_event.envelope.event
        else {
            panic!("explicit unavailable replacement must complete atomically");
        };
        assert_eq!(barrier.scope_epoch, 2);
        assert!(barrier.root_present);
        assert!(!barrier.explicit_object_errors.is_empty());
        assert!(barrier
            .explicit_object_errors
            .iter()
            .all(|error| error.code == "source_limit_exceeded"));
        assert!(barrier.source_coverage.iter().all(|coverage| {
            coverage.completeness == CoverageSetCompleteness::Unavailable
                && coverage.points.iter().all(|point| {
                    point.position.is_none()
                        && matches!(point.status, CoverageStatus::Unavailable { .. })
                })
        }));
        let barrier = Arc::clone(barrier);
        runtime
            .acknowledge_applied(completed_event.application_receipt())
            .unwrap();

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(
            &handle
                .with_attachment(|_, drain| drain.engine_resync_barrier())
                .unwrap(),
            &barrier
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let resumed_task =
            tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let resolution = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = resolution else {
            panic!("the retained terminal relation must still complete poll coverage");
        };
        assert_eq!(watermark.scope_epoch, 2);
        assert!(watermark
            .explicit_object_errors
            .iter()
            .all(|error| error.code == "source_limit_exceeded"));
        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_automatic_resync_publishes_first_access_retry_exhaustion() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("automatic-resync-exhaustion-root");
        let (mut runtime, handle, pair, target, drops) = automatic_single_object_pair(
            &registry,
            root,
            b"automatic-resync-exhaustion-session".to_vec(),
            AppendDelimitedConfig::json_lines(),
            128,
            ScopedObservationSourceOwnerRetryPolicy::new(
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                2,
            )
            .unwrap(),
        )
        .await;

        std::fs::write(&target, b"retry\n").unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));
        handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("continuity invalidation must yield a replacement handoff");
        };
        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            || 80,
            || 90,
        ));

        let required = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            required.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { .. }
        ));
        runtime
            .acknowledge_applied(required.application_receipt())
            .unwrap();
        let started = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            started.envelope.event,
            ScopedObservationEvent::ObserverResyncStarted { .. }
        ));
        runtime
            .acknowledge_applied(started.application_receipt())
            .unwrap();

        let scheduled = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::SourceObjectError { error } = &scheduled.envelope.event else {
            panic!("the first transient attempt must publish its retry schedule");
        };
        assert_eq!(
            scheduled.envelope.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert_eq!(
            error.retry,
            ScopedSourceObjectRetryState::RetryScheduled {
                failed_attempts: 1,
                max_attempts: 2,
                retry_after_ms: 1,
            }
        );
        assert!(error.provenance.last_successful_position.is_none());
        runtime
            .acknowledge_applied(scheduled.application_receipt())
            .unwrap();

        let exhausted = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::SourceObjectError { error } = &exhausted.envelope.event else {
            panic!("the bounded final attempt must publish retry exhaustion");
        };
        assert_eq!(
            exhausted.envelope.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert_eq!(
            error.retry,
            ScopedSourceObjectRetryState::RetryExhausted {
                failed_attempts: 2,
                max_attempts: 2,
            }
        );
        assert!(error.provenance.last_successful_position.is_none());
        runtime
            .acknowledge_applied(exhausted.application_receipt())
            .unwrap();

        let completed = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } = &completed.envelope.event
        else {
            panic!("first-access exhaustion must complete as explicit unavailable coverage");
        };
        assert!(barrier.root_present);
        assert!(barrier.source_coverage.iter().all(|coverage| {
            coverage.completeness == CoverageSetCompleteness::Unavailable
                && coverage.points.iter().all(|point| {
                    point.position.is_none()
                        && matches!(point.status, CoverageStatus::Unavailable { .. })
                })
        }));
        assert!(barrier
            .explicit_object_errors
            .iter()
            .all(|error| error.code == "decode_retry_transient"));
        runtime
            .acknowledge_applied(completed.application_receipt())
            .unwrap();

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(pair);
        assert!(runtime.close().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_automatic_resync_restarts_after_reoverflow_without_failing_observer() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("automatic-resync-reoverflow-root");
        let mut append_config = AppendDelimitedConfig::json_lines();
        append_config.max_record_bytes = 16;
        append_config.max_batch_bytes = 16;
        append_config.max_records_per_batch = 1;
        append_config.prefix_anchor_bytes = 8;
        let (mut runtime, handle, pair, target, drops) = automatic_single_object_pair(
            &registry,
            root,
            b"automatic-resync-reoverflow-session".to_vec(),
            append_config,
            128,
            ScopedObservationSourceOwnerRetryPolicy::new(
                std::time::Duration::from_millis(50),
                std::time::Duration::from_millis(50),
                2,
            )
            .unwrap(),
        )
        .await;

        // Epoch 2 admits one complete record, then parks on a bounded retry.
        // Re-overflow must supersede that incomplete stage and rebuild from
        // offset zero in epoch 3 without terminalizing the retained watcher.
        std::fs::write(&target, b"rec-01\nretry\n").unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));
        handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let joined_handle = handle.clone();
        let joined_resync = tokio::spawn(async move { joined_handle.resync_at(71).await.unwrap() });
        let joined_applied_handle = handle.clone();
        let joined_applied =
            tokio::spawn(async move { joined_applied_handle.resync_applied_at(72).await.unwrap() });
        tokio::task::yield_now().await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("continuity invalidation must yield a replacement handoff");
        };
        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            || 80,
            || 90,
        ));

        let required = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            required.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { .. }
        ));
        runtime
            .acknowledge_applied(required.application_receipt())
            .unwrap();
        let started = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            started.envelope.event,
            ScopedObservationEvent::ObserverResyncStarted { .. }
        ));
        assert_eq!(started.envelope.scope_epoch, 2);
        runtime
            .acknowledge_applied(started.application_receipt())
            .unwrap();

        let scheduled = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::SourceObjectError { error } = &scheduled.envelope.event else {
            panic!("the incomplete epoch must expose its relation-local retry");
        };
        assert_eq!(scheduled.envelope.scope_epoch, 2);
        assert_eq!(
            error.retry,
            ScopedSourceObjectRetryState::RetryScheduled {
                failed_attempts: 1,
                max_attempts: 2,
                retry_after_ms: 50,
            }
        );
        assert!(error
            .provenance
            .last_successful_position
            .as_ref()
            .is_some_and(|position| position.monotonic_order == Some(7)));
        assert!(!joined_resync.is_finished());
        assert!(!joined_applied.is_finished());

        let reoverflow = handle
            .require_resync(ScopedResyncReason::TransportContinuityLoss, 95)
            .unwrap();
        assert_eq!(reoverflow.invalid_scope_epoch, 2);
        std::fs::write(&target, b"rec-02\n").unwrap();
        runtime
            .acknowledge_applied(scheduled.application_receipt())
            .unwrap();

        let required_again = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &required_again.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { control }
                if Arc::ptr_eq(control, &reoverflow)
        ));
        assert_eq!(required_again.envelope.scope_epoch, 2);
        runtime
            .acknowledge_applied(required_again.application_receipt())
            .unwrap();
        let started_again = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncStarted { control } =
            &started_again.envelope.event
        else {
            panic!("the re-overflow must start one fresh replacement epoch");
        };
        assert_eq!(control.old_scope_epoch, 2);
        assert_eq!(control.new_scope_epoch, 3);
        assert_eq!(started_again.envelope.scope_epoch, 3);
        runtime
            .acknowledge_applied(started_again.application_receipt())
            .unwrap();

        let completed = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } = &completed.envelope.event
        else {
            panic!("the fresh epoch must finish without an observer failure");
        };
        assert_eq!(completed.envelope.scope_epoch, 3);
        assert_eq!(barrier.scope_epoch, 3);
        assert!(barrier.root_present);
        assert!(barrier.explicit_object_errors.is_empty());
        assert!(barrier.source_coverage.iter().all(|coverage| {
            coverage.completeness == CoverageSetCompleteness::Complete
                && coverage.points.iter().all(|point| {
                    point.position.as_ref().is_some_and(|position| {
                        position.kind == CoveragePositionKind::AppendCursor
                            && position.monotonic_order == Some(7)
                    })
                })
        }));
        let ScopedObservationResyncResolution::Ready(joined_barrier) =
            tokio::time::timeout(std::time::Duration::from_secs(2), joined_resync)
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("the original waiter must survive re-overflow to the later epoch");
        };
        assert!(Arc::ptr_eq(&joined_barrier, barrier));
        assert_eq!(joined_barrier.scope_epoch, 3);
        assert!(!joined_applied.is_finished());
        runtime
            .acknowledge_applied(completed.application_receipt())
            .unwrap();
        let ScopedObservationResyncResolution::Ready(joined_applied_barrier) =
            tokio::time::timeout(std::time::Duration::from_secs(2), joined_applied)
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("the applied waiter must survive re-overflow to the later epoch");
        };
        assert!(Arc::ptr_eq(&joined_applied_barrier, barrier));

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let resumed_task =
            tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let resolution = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = resolution else {
            panic!("the epoch-3 owner must service subsequent poll demand");
        };
        assert_eq!(watermark.scope_epoch, 3);
        assert!(watermark.explicit_object_errors.is_empty());

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_native_rescan_invalidates_live_epoch_and_reoverflows_after_start_offer() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-rescan-reoverflow-root");
        let (mut runtime, handle, pair, target, drops, callback_slot) =
            automatic_single_object_pair_with_watcher_policy(
                &registry,
                root,
                b"native-rescan-reoverflow-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                ScopedObservationSourceOwnerRetryPolicy::default(),
                ScopedObservationNativeWatcherRecoveryPolicy::new(
                    std::time::Duration::from_secs(60),
                    std::time::Duration::from_millis(1),
                    std::time::Duration::from_millis(1),
                    1,
                )
                .unwrap(),
            )
            .await;

        std::fs::write(&target, b"rec-01\n").unwrap();
        let poll_before = handle.host().poll_state();
        callback_slot.lock().unwrap().as_mut().unwrap()(Ok(notify::Event::new(
            notify::EventKind::Other,
        )
        .set_flag(notify::event::Flag::Rescan)));
        assert_eq!(handle.host().poll_state(), poll_before);

        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("a live native rescan must retain the owner pair for replacement");
        };
        assert!(matches!(
            handoff.source().exit(),
            ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(invalidation)
                if invalidation.control().unwrap().reason == ScopedResyncReason::WatcherOverflow
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        let watcher_loss_attempts = Arc::new(AtomicUsize::new(0));
        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            {
                let watcher_loss_attempts = Arc::clone(&watcher_loss_attempts);
                move || {
                    watcher_loss_attempts.fetch_add(1, Ordering::SeqCst);
                    80
                }
            },
            || 90,
        ));

        let required = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncRequired { control } = &required.envelope.event
        else {
            panic!("native rescan must first deliver its required control");
        };
        assert_eq!(control.reason, ScopedResyncReason::WatcherOverflow);
        assert_eq!(control.invalid_scope_epoch, 1);
        runtime
            .acknowledge_applied(required.application_receipt())
            .unwrap();

        let started = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let state = handle.with_attachment(|_, drain| {
                    let delivery = drain.delivery_lane();
                    (delivery.state(), delivery.resync_started())
                });
                if let (delivery, Some(started)) = state {
                    if delivery.continuity == ScopedObservationContinuity::Resyncing {
                        assert_eq!(delivery.scope_epoch, 2);
                        assert!(delivery.delivered_through_sequence < started.control_sequence);
                        break started;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        callback_slot.lock().unwrap().as_mut().unwrap()(Ok(notify::Event::new(
            notify::EventKind::Other,
        )
        .set_flag(notify::event::Flag::Rescan)));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while watcher_loss_attempts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        handle.with_attachment(|_, drain| {
            let delivery = drain.delivery_lane();
            let state = delivery.state();
            assert_eq!(state.continuity, ScopedObservationContinuity::Resyncing);
            assert_eq!(state.scope_epoch, 2);
            assert!(state.delivered_through_sequence < started.control_sequence);
        });

        let started_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncStarted {
            control: delivered_started,
        } = &started_event.envelope.event
        else {
            panic!("the first replacement start must remain delivery-owned");
        };
        assert!(Arc::ptr_eq(delivered_started, &started));
        runtime
            .acknowledge_applied(started_event.application_receipt())
            .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let state = handle.with_attachment(|_, drain| drain.delivery_lane().state());
                if state.continuity == ScopedObservationContinuity::ResyncRequired {
                    assert_eq!(state.scope_epoch, 2);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        acknowledge_clean_automatic_resync_epoch(
            &mut runtime,
            2,
            3,
            ScopedResyncReason::WatcherOverflow,
        )
        .await;

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let resumed_task =
            tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let resolution = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = resolution else {
            panic!("the epoch-3 owner must service later poll demand");
        };
        assert_eq!(watermark.scope_epoch, 3);
        assert!(watermark.explicit_object_errors.is_empty());

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_automatic_resync_replaces_present_root_with_complete_absence() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("automatic-resync-disappeared-root");
        let (mut runtime, handle, pair, target, drops, callback_slot) =
            automatic_single_object_pair_with_watcher_policy(
                &registry,
                root,
                b"automatic-resync-disappeared-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                ScopedObservationSourceOwnerRetryPolicy::default(),
                ScopedObservationNativeWatcherRecoveryPolicy::new(
                    std::time::Duration::from_secs(60),
                    std::time::Duration::from_millis(1),
                    std::time::Duration::from_millis(1),
                    1,
                )
                .unwrap(),
            )
            .await;

        std::fs::write(&target, b"rec-01\n").unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));
        let live = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(live) = live else {
            panic!("the present root must produce one complete live watermark");
        };
        assert_eq!(live.scope_epoch, 1);
        assert_eq!(live.scope_coverage.root_present(), Some(true));
        assert!(live.explicit_object_errors.is_empty());

        let mut saw_created = false;
        while runtime.applied_state().applied_through_sequence < live.offered_through_sequence {
            let event = runtime.next_event().await.unwrap().unwrap();
            assert_eq!(event.envelope.scope_epoch, 1);
            match &event.envelope.event {
                ScopedObservationEvent::SourcePresence {
                    change: ScopedAppendPresenceChange::Created { generation: 1 },
                } => saw_created = true,
                ScopedObservationEvent::ObserverFailed { .. } => {
                    panic!("present-root delivery must not fail the observer");
                }
                _ => {}
            }
            runtime
                .acknowledge_applied(event.application_receipt())
                .unwrap();
        }
        assert!(saw_created);

        std::fs::remove_file(&target).unwrap();
        callback_slot.lock().unwrap().as_mut().unwrap()(Ok(notify::Event::new(
            notify::EventKind::Other,
        )
        .set_flag(notify::event::Flag::Rescan)));
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("lost continuity must retain the present-root owner for replacement");
        };
        assert!(matches!(
            handoff.source().exit(),
            ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(invalidation)
                if invalidation.control().unwrap().reason == ScopedResyncReason::WatcherOverflow
        ));

        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            || 80,
            || 90,
        ));
        acknowledge_automatic_resync_start(&mut runtime, 1, 2, ScopedResyncReason::WatcherOverflow)
            .await;
        let completed = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } = &completed.envelope.event
        else {
            panic!("disappearance must finish through one replacement barrier");
        };
        assert_eq!(barrier.scope_epoch, 2);
        assert!(!barrier.root_present);
        assert_eq!(barrier.scope_coverage.root_present(), Some(false));
        assert!(matches!(
            &barrier.scope_coverage.relations()[0].state,
            ScopedScopeRelationState::Absent {
                kind: CoverageAbsenceKind::Absent
            }
        ));
        assert!(barrier.family_manifest.iter().all(|family| {
            family.completeness == CoverageSetCompleteness::Complete
                && family.entity_or_event_count == 0
        }));
        assert!(barrier.source_coverage.iter().all(|coverage| {
            coverage.completeness == CoverageSetCompleteness::Complete
                && coverage.points.is_empty()
                && coverage.explicit_absence_or_deletion.len() == 1
                && coverage.explicit_absence_or_deletion[0].kind == CoverageAbsenceKind::Absent
                && coverage.explicit_errors.is_empty()
        }));
        assert!(barrier.explicit_object_errors.is_empty());
        runtime
            .acknowledge_applied(completed.application_receipt())
            .unwrap();

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let resumed_task =
            tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let current = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(current) = current else {
            panic!("the absent-root epoch must remain a valid pollable snapshot");
        };
        assert_eq!(current.scope_epoch, 2);
        assert_eq!(current.scope_coverage.root_present(), Some(false));
        assert!(current.explicit_object_errors.is_empty());

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_automatic_resync_recovers_source_and_pair_binding_reoverflows() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("automatic-resync-bind-race-root");
        let (mut runtime, handle, pair, target, drops) = automatic_single_object_pair(
            &registry,
            root,
            b"automatic-resync-bind-race-session".to_vec(),
            AppendDelimitedConfig::json_lines(),
            128,
            ScopedObservationSourceOwnerRetryPolicy::default(),
        )
        .await;

        std::fs::write(&target, b"rec-01\n").unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));
        handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("continuity invalidation must yield a replacement handoff");
        };

        let before_source_binding = Arc::new(AtomicUsize::new(0));
        let before_owner_pair_binding = Arc::new(AtomicUsize::new(0));
        let recovery_task = tokio::spawn(
            handoff.replay_and_rebind_with_factory_clocks_and_boundaries(
                |_| Err(()),
                || 80,
                || 90,
                {
                    let before_source_binding = Arc::clone(&before_source_binding);
                    let target = target.clone();
                    move |handle| {
                        if before_source_binding
                            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                        {
                            std::fs::write(&target, b"rec-02\n").unwrap();
                            handle
                                .require_resync(ScopedResyncReason::ExplicitConsumerRequest, 91)
                                .unwrap();
                        }
                    }
                },
                {
                    let before_owner_pair_binding = Arc::clone(&before_owner_pair_binding);
                    let target = target.clone();
                    move |handle| {
                        if before_owner_pair_binding
                            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                        {
                            std::fs::write(&target, b"rec-03\n").unwrap();
                            handle
                                .require_resync(ScopedResyncReason::TransportContinuityLoss, 92)
                                .unwrap();
                        }
                    }
                },
            ),
        );

        acknowledge_automatic_resync_start(&mut runtime, 1, 2, ScopedResyncReason::WatcherOverflow)
            .await;
        acknowledge_automatic_resync_start(
            &mut runtime,
            2,
            3,
            ScopedResyncReason::ExplicitConsumerRequest,
        )
        .await;
        acknowledge_clean_automatic_resync_epoch(
            &mut runtime,
            3,
            4,
            ScopedResyncReason::TransportContinuityLoss,
        )
        .await;
        assert_eq!(before_source_binding.load(Ordering::SeqCst), 1);
        assert_eq!(before_owner_pair_binding.load(Ordering::SeqCst), 1);

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let resumed_task =
            tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let resolution = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = resolution else {
            panic!("the epoch-4 owner must service subsequent poll demand");
        };
        assert_eq!(watermark.scope_epoch, 4);
        assert!(watermark.explicit_object_errors.is_empty());

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_automatic_resync_retains_watcher_recovery_budget_across_reoverflow() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("automatic-resync-watcher-budget-root");
        let reinstall_attempts = Arc::new(AtomicUsize::new(0));
        let watcher_policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(500),
            std::time::Duration::from_secs(1),
            2,
        )
        .unwrap();
        let (mut runtime, handle, pair, target, drops, callback_slot) =
            automatic_single_object_pair_with_watcher_policy(
                &registry,
                root,
                b"automatic-resync-watcher-budget-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                ScopedObservationSourceOwnerRetryPolicy::default(),
                watcher_policy,
            )
            .await;

        std::fs::write(&target, b"rec-01\n").unwrap();
        callback_slot.lock().unwrap().as_mut().unwrap()(Err(notify::Error::generic(
            "fixture backend disconnected during replacement",
        )));
        handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 71, || 72));
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("continuity invalidation must retain the failed watcher for replacement");
        };

        let recovery_task = tokio::spawn(
            handoff.replay_and_rebind_with_factory_clocks_and_boundaries(
                {
                    let reinstall_attempts = Arc::clone(&reinstall_attempts);
                    move |_| {
                        reinstall_attempts.fetch_add(1, Ordering::SeqCst);
                        Err(())
                    }
                },
                || 80,
                || 90,
                {
                    let handle = handle.clone();
                    let reinstall_attempts = Arc::clone(&reinstall_attempts);
                    move |_| {
                        assert_eq!(reinstall_attempts.load(Ordering::SeqCst), 1);
                        handle
                            .require_resync(ScopedResyncReason::TransportContinuityLoss, 95)
                            .unwrap();
                    }
                },
                |_| {},
            ),
        );

        let required = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            required.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { .. }
        ));
        runtime
            .acknowledge_applied(required.application_receipt())
            .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while reinstall_attempts.load(Ordering::SeqCst) < 1 {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(reinstall_attempts.load(Ordering::SeqCst), 1);
        let started = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            started.envelope.event,
            ScopedObservationEvent::ObserverResyncStarted { .. }
        ));
        assert_eq!(started.envelope.scope_epoch, 2);
        runtime
            .acknowledge_applied(started.application_receipt())
            .unwrap();

        let failure = tokio::time::timeout(std::time::Duration::from_secs(3), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            failure.error(),
            ScopedObservationAutomaticResyncError::WatcherStopped
        );
        assert_eq!(reinstall_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            failure.terminal_failure().unwrap().reason,
            ScopedObserverFailureReason::NativeWatcherRecoveryExhausted
        );

        let failed = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverFailed { failure } = &failed.envelope.event else {
            panic!("the retained cumulative watcher budget must end in one terminal control");
        };
        assert_eq!(
            failure.reason,
            ScopedObserverFailureReason::NativeWatcherRecoveryExhausted
        );
        runtime
            .acknowledge_applied(failed.application_receipt())
            .unwrap();
        let close = runtime.request_close();
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_automatic_resync_rejects_active_watcher_policy_drift() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp
            .path()
            .join("automatic-resync-watcher-policy-drift-root");
        let original_policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(100),
            1,
        )
        .unwrap();
        let drifted_policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(1),
            4,
        )
        .unwrap();
        let (mut runtime, handle, pair, target, drops, callback_slot) =
            automatic_single_object_pair_with_watcher_policy(
                &registry,
                root,
                b"automatic-resync-watcher-policy-drift-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                ScopedObservationSourceOwnerRetryPolicy::default(),
                original_policy,
            )
            .await;

        std::fs::write(&target, b"rec-01\n").unwrap();
        callback_slot.lock().unwrap().as_mut().unwrap()(Err(notify::Error::generic(
            "fixture backend disconnected before policy drift",
        )));
        handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let result = pair
            .run_with_factory_and_clocks(|_| Err(()), || 71, || 72)
            .await;
        let ScopedObservationAsyncOwnerRunResult::Resync(mut handoff) = result else {
            panic!("continuity invalidation must retain the active watcher incident");
        };
        handoff.replace_watcher_policy_for_test(drifted_policy);

        let failure = handoff
            .replay_and_rebind_with_factory_and_clocks(|_| Err(()), || 80, || 90)
            .await
            .unwrap_err();
        assert_eq!(
            failure.error(),
            ScopedObservationAutomaticResyncError::WatcherStopped
        );
        assert_eq!(
            failure.terminal_failure().unwrap().reason,
            ScopedObserverFailureReason::InternalControlFailure
        );
        let failed = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &failed.envelope.event,
            ScopedObservationEvent::ObserverFailed { failure }
                if failure.reason == ScopedObserverFailureReason::InternalControlFailure
        ));
        runtime
            .acknowledge_applied(failed.application_receipt())
            .unwrap();
        let close = runtime.request_close();
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_automatic_resync_rolls_back_terminal_error_after_partial_object_progress() {
        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("automatic-resync-partial-error-root");
        let mut append_config = AppendDelimitedConfig::json_lines();
        append_config.max_record_bytes = 16;
        append_config.max_batch_bytes = 16;
        append_config.max_records_per_batch = 1;
        append_config.prefix_anchor_bytes = 8;
        let (mut runtime, handle, pair, target, drops) = automatic_single_object_pair(
            &registry,
            root.clone(),
            b"automatic-resync-partial-error-session".to_vec(),
            append_config,
            128,
            ScopedObservationSourceOwnerRetryPolicy::new(
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                2,
            )
            .unwrap(),
        )
        .await;

        // Batch one commits an exact replacement cursor and semantic entity.
        // Batch two remains transient through the bounded retry ceiling. The
        // final attempt rolls back only this object's cursor, coverage, and
        // reducer contributions, then publishes terminal Unavailable coverage
        // without the prefix.
        std::fs::write(&target, b"rec-01\nretry\n").unwrap();
        let pair_task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 60, || 61));
        handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pair_task)
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = result else {
            panic!("continuity invalidation must yield a replacement handoff");
        };
        let recovery_task = tokio::spawn(handoff.replay_and_rebind_with_factory_and_clocks(
            |_| Err(()),
            || 80,
            || 90,
        ));
        let required_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            required_event.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { .. }
        ));
        runtime
            .acknowledge_applied(required_event.application_receipt())
            .unwrap();
        let started_event = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            started_event.envelope.event,
            ScopedObservationEvent::ObserverResyncStarted { .. }
        ));
        runtime
            .acknowledge_applied(started_event.application_receipt())
            .unwrap();

        let scheduled_error_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::SourceObjectError { error } =
            &scheduled_error_event.envelope.event
        else {
            panic!("partial replacement retry must remain object-local");
        };
        assert_eq!(
            error.failure_code,
            ScopedSourceObjectFailureCode::DecodeRetryTransient
        );
        assert_eq!(
            error.retry,
            ScopedSourceObjectRetryState::RetryScheduled {
                failed_attempts: 1,
                max_attempts: 2,
                retry_after_ms: 1,
            }
        );
        assert!(error
            .provenance
            .last_successful_position
            .as_ref()
            .is_some_and(|position| {
                position.kind == CoveragePositionKind::AppendCursor
                    && position.monotonic_order == Some(7)
            }));
        runtime
            .acknowledge_applied(scheduled_error_event.application_receipt())
            .unwrap();

        let exhausted_error_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::SourceObjectError { error } =
            &exhausted_error_event.envelope.event
        else {
            panic!("partial replacement retry exhaustion must remain object-local");
        };
        assert_eq!(
            error.failure_code,
            ScopedSourceObjectFailureCode::DecodeRetryTransient
        );
        assert_eq!(
            error.retry,
            ScopedSourceObjectRetryState::RetryExhausted {
                failed_attempts: 2,
                max_attempts: 2,
            }
        );
        assert!(error
            .provenance
            .last_successful_position
            .as_ref()
            .is_some_and(|position| position.monotonic_order == Some(7)));
        runtime
            .acknowledge_applied(exhausted_error_event.application_receipt())
            .unwrap();

        let completed_event = runtime.next_event().await.unwrap().unwrap();
        let ScopedObservationEvent::ObserverResyncComplete { barrier } =
            &completed_event.envelope.event
        else {
            panic!("rolled-back object failure must complete the replacement");
        };
        assert_eq!(barrier.scope_epoch, 2);
        assert!(barrier.root_present);
        assert_eq!(barrier.family_manifest.len(), 1);
        assert!(barrier.family_manifest.iter().all(|family| {
            family.entity_or_event_count == 0
                && family.completeness == CoverageSetCompleteness::Unavailable
        }));
        assert!(!barrier.source_coverage.is_empty());
        assert!(barrier.source_coverage.iter().all(|coverage| {
            coverage.completeness == CoverageSetCompleteness::Unavailable
                && coverage.points.iter().all(|point| {
                    point.position.is_none()
                        && matches!(point.status, CoverageStatus::Unavailable { .. })
                })
        }));
        assert!(barrier
            .explicit_object_errors
            .iter()
            .all(|error| error.code == "decode_retry_transient"));
        runtime
            .acknowledge_applied(completed_event.application_receipt())
            .unwrap();

        let pair = tokio::time::timeout(std::time::Duration::from_secs(2), recovery_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let resumed_task =
            tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        let watermark = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = watermark else {
            panic!("rolled-back terminal state must remain pollable");
        };
        assert_eq!(watermark.scope_epoch, 2);
        assert!(watermark.source_coverage.iter().all(|coverage| {
            coverage.completeness == CoverageSetCompleteness::Unavailable
                && coverage.points.iter().all(|point| point.position.is_none())
        }));
        let redacted = format!("{watermark:?}");
        assert!(!redacted.contains(root.to_string_lossy().as_ref()));
        assert!(!redacted.contains("automatic-resync-partial-error-session"));
        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn scoped_source_owner_isolates_object_retry_exhaustion_from_healthy_siblings() {
        let registry = stateful_two_object_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("object-isolated-retry-root");
        std::fs::create_dir_all(&root).unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            two_object_scoped_access_request(root.clone()),
        )
        .unwrap();
        let mut runtime = ScopedObservationAsyncRuntime::open(
            host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 16,
                max_retained_native_bytes: 0,
                max_source_control_items: 16,
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let mut objects = vec![
            scoped_append_object_for_native_object(b"session.jsonl"),
            scoped_append_object_for_native_object(b"sibling.jsonl"),
        ];
        let root_source = objects[0].source_identity().clone();
        let sibling_source = objects[1].source_identity().clone();
        let mut admission = admission_lane_for_objects(2);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 16,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"object-isolated-session",
        }];
        let root_origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let sibling_origin = RecordOrigin {
            object_id: 31,
            ..root_origin.clone()
        };

        let bootstrap_ticket = handle.host().request_poll().unwrap();
        let bootstrap_lease = handle.host().begin_poll().unwrap().unwrap();
        reconcile_missing_relation_poll(
            handle.host(),
            &bootstrap_lease,
            "root-object",
            &mut objects[0],
            &mut admission,
            &identity,
            &root_origin,
        );
        reconcile_missing_relation_poll(
            handle.host(),
            &bootstrap_lease,
            "sibling-object",
            &mut objects[1],
            &mut admission,
            &identity,
            &sibling_origin,
        );
        let bootstrap_watermark = handle.with_attachment(|host, drain| {
            host.complete_bootstrap_poll(bootstrap_lease, &admission, &projection, drain)
                .unwrap()
        });
        assert!(matches!(
            bootstrap_ticket.wait_async().await,
            ScopedObservationPollResolution::Ready(watermark)
                if Arc::ptr_eq(&watermark, &bootstrap_watermark)
        ));
        for object in &mut objects {
            object.complete_bootstrap().unwrap();
        }
        let barrier = handle.with_attachment(|host, drain| {
            host.offer_consumer_bootstrap_complete(&objects, &admission, &projection, drain, 50)
                .unwrap()
        });
        let bootstrap = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &bootstrap.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { barrier: delivered }
                if Arc::ptr_eq(delivered, &barrier)
        ));
        runtime
            .acknowledge_applied(bootstrap.application_receipt())
            .unwrap();

        let active = handle
            .with_attachment(|host, drain| {
                host.bind_consumer_bootstrap_epoch_state(objects, admission, projection, drain)
            })
            .unwrap();
        let bindings = vec![
            ScopedObservationAppendPassBinding::new(
                "root-object",
                vec![ScopedObservationOwnedIdentityInput::new(
                    "native-session-id",
                    b"object-isolated-session".to_vec(),
                )
                .unwrap()],
                None,
                1,
                64,
                root_origin,
                false,
            )
            .unwrap(),
            ScopedObservationAppendPassBinding::new(
                "sibling-object",
                vec![ScopedObservationOwnedIdentityInput::new(
                    "native-session-id",
                    b"object-isolated-session".to_vec(),
                )
                .unwrap()],
                None,
                1,
                64,
                sibling_origin,
                false,
            )
            .unwrap(),
        ];
        let owner = handle
            .bind_epoch_source_owner(
                active,
                bindings,
                ScopedObservationSourceOwnerRetryPolicy::new(
                    std::time::Duration::from_millis(2),
                    std::time::Duration::from_millis(2),
                    2,
                )
                .unwrap(),
            )
            .unwrap();
        let pass_count = Arc::new(AtomicUsize::new(0));
        let source_pass_count = Arc::clone(&pass_count);
        let source_task = tokio::spawn(owner.run_until_stopped_with_clock(move || {
            100 + source_pass_count.fetch_add(1, Ordering::SeqCst) as i64
        }));

        std::fs::write(root.join("session.jsonl"), b"retry\n").unwrap();
        std::fs::write(root.join("sibling.jsonl"), b"healthy\n").unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(first, ScopedObservationPollResolution::Ready(_)));

        let mut retry_states = Vec::new();
        let mut sibling_created = false;
        while retry_states.len() < 2 || !sibling_created {
            let yielded =
                tokio::time::timeout(std::time::Duration::from_secs(2), runtime.next_event())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            match &yielded.envelope.event {
                ScopedObservationEvent::SourceObjectError { error } => {
                    assert_eq!(error.relation_id.as_ref(), "root-object");
                    assert_eq!(error.provenance.generation, 1);
                    assert!(error.provenance.last_successful_position.is_none());
                    retry_states.push(error.retry);
                    let redacted = format!("{error:?}");
                    assert!(!redacted.contains(root.to_string_lossy().as_ref()));
                    assert!(!redacted.contains("object-isolated-session"));
                }
                ScopedObservationEvent::SourcePresence {
                    change: ScopedAppendPresenceChange::Created { generation: 1 },
                } if yielded.envelope.source.object_key == sibling_source.object_key => {
                    sibling_created = true;
                }
                _ => {}
            }
            runtime
                .acknowledge_applied(yielded.application_receipt())
                .unwrap();
        }
        assert!(matches!(
            retry_states.as_slice(),
            [
                ScopedSourceObjectRetryState::RetryScheduled {
                    failed_attempts: 1,
                    max_attempts: 2,
                    retry_after_ms: 2,
                },
                ScopedSourceObjectRetryState::RetryExhausted {
                    failed_attempts: 2,
                    max_attempts: 2,
                },
            ]
        ));
        assert!(!source_task.is_finished());

        let passes_after_exhaustion = pass_count.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(pass_count.load(Ordering::SeqCst), passes_after_exhaustion);

        OpenOptions::new()
            .append(true)
            .open(root.join("sibling.jsonl"))
            .unwrap()
            .write_all(b"more\n")
            .unwrap();
        let sibling_progress =
            tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
                .await
                .unwrap()
                .unwrap();
        let ScopedObservationPollResolution::Ready(watermark) = sibling_progress else {
            panic!("healthy sibling poll must complete through terminal object state");
        };
        assert!(watermark.explicit_object_errors.iter().any(|error| {
            error.object_key == Some(root_source.object_key)
                && error.code == "decode_retry_transient"
        }));

        OpenOptions::new()
            .append(true)
            .open(root.join("sibling.jsonl"))
            .unwrap()
            .write_all(b"stream-fatal\n")
            .unwrap();
        let terminal_sibling =
            tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            terminal_sibling,
            ScopedObservationPollResolution::Ready(_)
        ));
        let terminal =
            tokio::time::timeout(std::time::Duration::from_secs(2), runtime.next_event())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        let ScopedObservationEvent::SourceObjectError { error } = &terminal.envelope.event else {
            panic!("stream-fatal decode must become an object-local terminal control");
        };
        assert_eq!(error.relation_id.as_ref(), "sibling-object");
        assert_eq!(error.source, sibling_source);
        assert!(matches!(
            error.retry,
            ScopedSourceObjectRetryState::NotRetryable { failed_attempts: 1 }
        ));
        assert_eq!(
            error
                .provenance
                .last_successful_position
                .as_ref()
                .and_then(|position| position.monotonic_order),
            Some(13)
        );
        runtime
            .acknowledge_applied(terminal.application_receipt())
            .unwrap();
        assert!(!source_task.is_finished());

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), source_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped.exit(),
            ScopedObservationSourceOwnerRunExit::Cancelled
        ));
        let (active, _) = stopped.into_parts();
        assert!(matches!(
            active.object_error("root-object").map(|error| error.retry),
            Some(ScopedSourceObjectRetryState::RetryExhausted {
                failed_attempts: 2,
                max_attempts: 2,
            })
        ));
        assert!(matches!(
            active
                .object_error("sibling-object")
                .map(|error| error.retry),
            Some(ScopedSourceObjectRetryState::NotRetryable { failed_attempts: 1 })
        ));
        assert_eq!(
            active
                .append_object("sibling-object")
                .unwrap()
                .checkpoint()
                .unwrap()
                .committed_offset,
            13
        );
        assert!(active
            .append_object("root-object")
            .unwrap()
            .checkpoint()
            .is_none());
        assert!(close.wait_async().await.complete);
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
                        emit_rescan: true,
                    }))
                }
            })
            .unwrap();
        assert_eq!(watcher.watch_anchor_count(), 1);
        assert_eq!(
            watcher.coordinator().phase(),
            ScopedObservationWatcherPhase::WatcherInstalled
        );
        assert_eq!(watcher.state().generation, 2);
        assert!(!watcher.state().backend_failed);
        assert!(!watcher.state().routing_failed);
        assert!(!watcher.state().continuity_loss_pending());
        let signalled = watcher.waiter().wait_after(0).await;
        assert_eq!(signalled.generation, 2);
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
                    emit_rescan: false,
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
                        emit_rescan: false,
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

    #[tokio::test]
    async fn scoped_native_watcher_audit_and_backend_reinstallation_are_retry_safe() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-watch-reinstall-root");
        std::fs::create_dir_all(&root).unwrap();
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
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut watcher = handle
            .install_native_watcher_with_factory(2, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        assert_eq!(watcher.state().backend_generation, 1);
        assert_eq!(watcher.state().generation, 0);

        assert!(matches!(
            watcher.request_audit().unwrap(),
            ScopedObservationWatcherHintAction::Buffered(HintEnqueue::EscalatedToInstance)
        ));
        let before_failure = watcher.state();
        assert_eq!(before_failure.generation, 1);
        callback_slot.lock().unwrap().as_mut().unwrap()(Err(notify::Error::generic(
            "fixture backend disconnected",
        )));
        let failed = watcher.waiter().wait_after(before_failure.generation).await;
        assert!(failed.backend_failed);
        assert!(!failed.reinstalling);
        assert_eq!(failed.backend_generation, 1);

        assert!(matches!(
            watcher.reinstall_native_backend_with_factory(|_| Err(())),
            Err(ScopedObservationNativeWatcherError::BackendUnavailable)
        ));
        let failed_reinstall = watcher.state();
        assert!(failed_reinstall.backend_failed);
        assert!(!failed_reinstall.reinstalling);
        assert_eq!(failed_reinstall.backend_generation, 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(callback_slot.lock().unwrap().is_none());

        assert!(matches!(
            watcher
                .reinstall_native_backend_with_factory({
                    let callback_slot = Arc::clone(&callback_slot);
                    let registrations = Arc::clone(&registrations);
                    let drops = Arc::clone(&drops);
                    move |callback| {
                        Ok(Box::new(ControlledScopedWatchBackend {
                            callback: Some(callback),
                            callback_slot,
                            registrations,
                            drops,
                        }))
                    }
                })
                .unwrap(),
            ScopedObservationWatcherHintAction::Buffered(HintEnqueue::Coalesced)
        ));
        let recovered = watcher.state();
        assert_eq!(recovered.backend_generation, 2);
        assert!(!recovered.backend_failed);
        assert!(!recovered.routing_failed);
        assert!(!recovered.reinstalling);
        assert_eq!(registrations.lock().unwrap().len(), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        let barrier = runtime.request_close();
        drop(watcher);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(barrier.wait_async().await.complete);
    }

    #[tokio::test]
    async fn scoped_native_watcher_recovery_loop_reinstalls_and_resumes() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-watch-recovery-loop-root");
        std::fs::create_dir_all(&root).unwrap();
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
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let watcher = handle
            .install_native_watcher_with_factory(1, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        callback_slot.lock().unwrap().as_mut().unwrap()(Err(notify::Error::generic(
            "fixture backend disconnected",
        )));
        let failed = watcher.state();
        assert!(failed.backend_failed);
        let waiter = watcher.waiter();

        let policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            1,
        )
        .unwrap();
        let runner = tokio::spawn(watcher.run_with_recovery_with_factory_and_clock(
            policy,
            {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot: Arc::clone(&callback_slot),
                        registrations: Arc::clone(&registrations),
                        drops: Arc::clone(&drops),
                    }))
                }
            },
            || 321,
        ));
        let recovered = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let state = waiter.state();
                if state.backend_generation == 2 && !state.backend_failed && !state.reinstalling {
                    break state;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!recovered.routing_failed);
        assert_eq!(registrations.lock().unwrap().len(), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        callback_slot.lock().unwrap().as_mut().unwrap()(Err(notify::Error::generic(
            "fixture replacement backend disconnected",
        )));
        let recovered_again = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let state = waiter.state();
                if state.backend_generation == 3 && !state.backend_failed && !state.reinstalling {
                    break state;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!recovered_again.routing_failed);
        assert_eq!(registrations.lock().unwrap().len(), 3);
        assert_eq!(drops.load(Ordering::SeqCst), 2);

        let barrier = handle.request_close();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), runner)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            ScopedObservationNativeWatcherRunExit::Cancelled
        );
        assert_eq!(drops.load(Ordering::SeqCst), 3);
        assert!(barrier.wait_async().await.complete);
    }

    #[tokio::test]
    async fn scoped_native_watcher_recovery_exhaustion_delivers_failure_without_closing() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-watch-exhaustion-root");
        std::fs::create_dir_all(&root).unwrap();
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
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let watcher = handle
            .install_native_watcher_with_factory(1, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        callback_slot.lock().unwrap().as_mut().unwrap()(Err(notify::Error::generic(
            "fixture backend permanently unavailable",
        )));
        assert!(watcher.state().backend_failed);

        let policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            2,
        )
        .unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let exit = watcher
            .run_with_recovery_with_factory_and_clock(
                policy,
                {
                    let attempts = Arc::clone(&attempts);
                    move |_| {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Err(())
                    }
                },
                || 123,
            )
            .await
            .unwrap();
        let ScopedObservationNativeWatcherRunExit::Failed(failure) = exit else {
            panic!("permanent backend failure must exhaust into observer failure");
        };
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            failure.reason,
            ScopedObserverFailureReason::NativeWatcherRecoveryExhausted
        );
        assert!(!handle.host().is_closed());
        assert!(handle.host().poll_state().failed);

        let yielded = runtime.next_event().await.unwrap().unwrap();
        assert_eq!(yielded.envelope.observed_at, 123);
        assert!(matches!(
            &yielded.envelope.event,
            ScopedObservationEvent::ObserverFailed {
                failure: delivered,
            } if Arc::ptr_eq(delivered, &failure)
        ));
        runtime
            .acknowledge_applied(yielded.application_receipt())
            .unwrap();
        assert!(runtime.close().await.complete);
    }

    #[tokio::test]
    async fn scoped_native_watcher_drop_preserves_attachment_terminal_failure() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-watch-external-failure-root");
        std::fs::create_dir_all(&root).unwrap();
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
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let watcher = handle
            .install_native_watcher_with_factory(1, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();

        let failure = handle
            .fail_observer(ScopedObserverFailureReason::InternalControlFailure, 654)
            .unwrap();
        drop(watcher);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!handle.host().is_closed());

        let yielded = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &yielded.envelope.event,
            ScopedObservationEvent::ObserverFailed {
                failure: delivered,
            } if Arc::ptr_eq(delivered, &failure)
        ));
        runtime
            .acknowledge_applied(yielded.application_receipt())
            .unwrap();
        assert!(runtime.close().await.complete);
    }

    #[tokio::test]
    async fn scoped_native_watcher_recovery_loop_schedules_audits_and_cancels() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-watch-audit-loop-root");
        std::fs::create_dir_all(&root).unwrap();
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
        let callback_slot = Arc::new(std::sync::Mutex::new(None));
        let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicUsize::new(0));
        let watcher = handle
            .install_native_watcher_with_factory(1, {
                let callback_slot = Arc::clone(&callback_slot);
                let registrations = Arc::clone(&registrations);
                let drops = Arc::clone(&drops);
                move |callback| {
                    Ok(Box::new(ControlledScopedWatchBackend {
                        callback: Some(callback),
                        callback_slot,
                        registrations,
                        drops,
                    }))
                }
            })
            .unwrap();
        let waiter = watcher.waiter();
        let policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            1,
        )
        .unwrap();
        let runner = tokio::spawn(watcher.run_with_recovery_with_factory_and_clock(
            policy,
            |_| Err(()),
            || 456,
        ));
        let audited = tokio::time::timeout(std::time::Duration::from_secs(2), waiter.wait_after(0))
            .await
            .unwrap();
        assert!(audited.generation > 0);
        assert_eq!(audited.backend_generation, 1);
        assert!(!audited.backend_failed);
        assert!(!audited.routing_failed);

        let barrier = handle.request_close();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), runner)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            ScopedObservationNativeWatcherRunExit::Cancelled
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(barrier.wait_async().await.complete);
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
                max_source_control_items: 1,
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

        // The already-offered presence control fills the one-item source
        // control lane. Atomic bootstrap completion must roll the object back
        // before releasing the startup lock, so a callback racing the retry
        // remains startup reconciliation rather than being mislabeled Live.
        assert!(matches!(
            startup.complete_and_offer_bootstrap(
                &host,
                std::slice::from_mut(&mut object),
                &admission,
                &projection,
                &mut drain,
                50,
            ),
            Err(ScopedObservationStartupError::Bootstrap(
                ScopedBootstrapBarrierError::Delivery(ScopedDeliveryError::SourceControlQueueFull)
            ))
        ));
        assert!(object.bootstrap_active());
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

        let presence = drain.next().unwrap().unwrap();
        assert!(matches!(
            presence.envelope.event,
            ScopedObservationEvent::SourcePresence { .. }
        ));
        drain
            .acknowledge_applied(presence.application_receipt())
            .unwrap();
        let barrier = startup
            .complete_and_offer_bootstrap(
                &host,
                std::slice::from_mut(&mut object),
                &admission,
                &projection,
                &mut drain,
                50,
            )
            .unwrap();
        assert!(!object.bootstrap_active());
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
        let duplicate_ticket = match startup
            .record_hint(
                &host,
                DirtyHint {
                    scope: DirtyScope::Object(b"root-object".to_vec()),
                    reason: DirtyReason::NativeEvent,
                },
            )
            .unwrap()
        {
            ScopedObservationWatcherHintAction::PollRequested { ticket, .. } => ticket,
            ScopedObservationWatcherHintAction::Buffered(_) => {
                panic!("a duplicate live callback must remain in live poll scheduling")
            }
        };
        assert_eq!(
            duplicate_ticket.request_generation(),
            live_ticket.request_generation(),
            "callbacks before pass reservation must share one exact-scope demand"
        );
        let lease = host.begin_poll().unwrap().unwrap();
        let during_pass_ticket = match startup
            .record_hint(
                &host,
                DirtyHint {
                    scope: DirtyScope::Object(b"root-object".to_vec()),
                    reason: DirtyReason::NativeEvent,
                },
            )
            .unwrap()
        {
            ScopedObservationWatcherHintAction::PollRequested { ticket, .. } => ticket,
            ScopedObservationWatcherHintAction::Buffered(_) => {
                panic!("a callback racing a live pass must schedule its follow-up")
            }
        };
        assert_eq!(
            during_pass_ticket.request_generation(),
            live_ticket.request_generation() + 1,
            "an active pass cannot acknowledge a callback that raced its reads"
        );
        assert!(matches!(
            host.execute_epoch_poll_pass(lease, &mut active, &mut drain, &[]),
            Err(ScopedObservationPassExecutionError::InvalidRelationSet)
        ));
        assert_eq!(
            host.poll_resolution(&live_ticket).unwrap(),
            ScopedObservationPollResolution::Pending
        );

        let pass_request = [ScopedObservationAppendPassRequest {
            relation_id: "root-object",
            identity_inputs: &identity,
            parent_token: None,
            depth: 1,
            max_bytes: 64,
            origin: &origin,
            force_contract_replay: false,
        }];
        let pass_debug = format!("{pass_request:?}");
        assert!(pass_debug.contains("native-session-id"));
        assert!(!pass_debug.contains("watch-before-scan-session"));

        // Canonical root equality is not attachment ownership. A second host
        // authorized for the same native root cannot drive or mutate this
        // host's active epoch state.
        let foreign =
            ScopedObservationAccessHost::authorize(&registry, scoped_access_request(root.clone()))
                .unwrap();
        assert_eq!(foreign.root_identity(), host.root_identity());
        let mut foreign_drain = foreign
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 4,
                max_retained_native_bytes: 0,
                max_source_control_items: 2,
            })
            .unwrap();
        let foreign_ticket = foreign.request_poll().unwrap();
        let foreign_lease = foreign.begin_poll().unwrap().unwrap();
        assert!(matches!(
            foreign.execute_epoch_poll_pass(
                foreign_lease,
                &mut active,
                &mut foreign_drain,
                &pass_request,
            ),
            Err(ScopedObservationPassExecutionError::InvalidEpochState)
        ));
        assert_eq!(
            foreign.poll_resolution(&foreign_ticket).unwrap(),
            ScopedObservationPollResolution::Pending
        );
        let foreign_close = foreign.close_with_consumer(&mut foreign_drain).unwrap();
        assert!(foreign_close.wait().complete);
        assert_eq!(
            foreign_ticket.wait(),
            ScopedObservationPollResolution::Cancelled
        );

        let retry = host.begin_poll().unwrap().unwrap();
        let live_watermark = host
            .execute_epoch_poll_pass(retry, &mut active, &mut drain, &pass_request)
            .unwrap();
        assert!(active
            .append_object("root-object")
            .expect("the bound root relation remains active")
            .is_present());
        assert!(active.admission_is_empty());
        assert_eq!(live_watermark.offered_through_sequence, 2);
        assert!(matches!(
            host.poll_resolution(&live_ticket).unwrap(),
            ScopedObservationPollResolution::Ready(watermark)
                if Arc::ptr_eq(&watermark, &live_watermark)
        ));
        for coalesced in [&duplicate_ticket, &during_pass_ticket] {
            assert!(matches!(
                host.poll_resolution(coalesced).unwrap(),
                ScopedObservationPollResolution::Ready(watermark)
                    if Arc::ptr_eq(&watermark, &live_watermark)
            ));
        }

        // The bootstrap control still fills the one-item bounded control
        // lane. Admission may commit the later deletion cursor, but a failed
        // offer must keep the poll pending and the deletion queued.
        std::fs::remove_file(root.join("session.jsonl")).unwrap();
        let deletion_ticket = host.request_poll().unwrap();
        let deletion_lease = host.begin_poll().unwrap().unwrap();
        assert!(matches!(
            host.execute_epoch_poll_pass(deletion_lease, &mut active, &mut drain, &pass_request,),
            Err(ScopedObservationPassExecutionError::Offer(
                ScopedObservationConsumerOfferError::Offer(
                    ScopedProjectionDeliveryError::Delivery(
                        ScopedDeliveryError::SourceControlQueueFull
                    )
                )
            ))
        ));
        assert!(!active
            .append_object("root-object")
            .expect("the root relation remains bound after deletion")
            .is_present());
        assert!(!active.admission_is_empty());
        assert_eq!(
            host.poll_resolution(&deletion_ticket).unwrap(),
            ScopedObservationPollResolution::Pending
        );

        let bootstrap_delivery = drain.next().unwrap().unwrap();
        assert!(matches!(
            bootstrap_delivery.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { .. }
        ));
        drain
            .acknowledge_applied(bootstrap_delivery.application_receipt())
            .unwrap();

        // Retrying first flushes the already-admitted deletion, then takes a
        // fresh exact pass so completion cannot reuse the abandoned pass ID.
        let deletion_retry = host.begin_poll().unwrap().unwrap();
        let deletion_watermark = host
            .execute_epoch_poll_pass(deletion_retry, &mut active, &mut drain, &pass_request)
            .unwrap();
        assert!(active.admission_is_empty());
        assert_eq!(deletion_watermark.offered_through_sequence, 3);
        assert!(matches!(
            host.poll_resolution(&deletion_ticket).unwrap(),
            ScopedObservationPollResolution::Ready(watermark)
                if Arc::ptr_eq(&watermark, &deletion_watermark)
        ));

        let retained_ready = host.ready_waiter().unwrap();
        let closing_events = drain.event_waiter();
        let before_close = closing_events.snapshot();
        assert_eq!(before_close.offered_through_sequence, 3);
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
            deletion_ticket.wait(),
            ScopedObservationPollResolution::Ready(watermark)
                if Arc::ptr_eq(&watermark, &deletion_watermark)
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
    fn scoped_host_requires_the_declared_root_object_designation() {
        let registry = supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let mut request = scoped_access_request(temp.path().join("missing-root-role"));
        request.known_objects[0].scope_root = false;

        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, request),
            Err(ScopedObservationAccessError::InvalidGrant(message))
                if message.contains("authorized scope root relation")
        ));
    }

    #[test]
    fn scoped_host_requires_every_declared_known_object_and_rejects_root_swaps() {
        let registry = stateful_two_object_fixture_registry();
        let temp = TempDir::new().unwrap();

        let omitted = scoped_access_request(temp.path().join("omitted-known-object"));
        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, omitted),
            Err(ScopedObservationAccessError::InvalidGrant(message))
                if message.contains("must equal every declared KnownObject relation")
        ));

        let mut swapped = two_object_scoped_access_request(temp.path().join("swapped-root-object"));
        swapped.known_objects[0].scope_root = false;
        swapped.known_objects[1].scope_root = true;
        assert!(matches!(
            ScopedObservationAccessHost::authorize(&registry, swapped),
            Err(ScopedObservationAccessError::InvalidGrant(message))
                if message.contains("authorized scope root relation")
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
        let missing_scope = &missing_watermark.scope_coverage;
        assert_eq!(missing_scope.contract_version(), 1);
        assert_eq!(missing_scope.program_id(), "observe-session");
        assert_eq!(missing_scope.root_relation_id(), "root-object");
        assert_eq!(
            missing_scope.scope_program_digest(),
            Sha256Digest::of(SINGLE_OBJECT_SCOPE_DOCUMENT)
        );
        assert_eq!(
            missing_scope.completeness(),
            CoverageSetCompleteness::Complete
        );
        assert_eq!(missing_scope.root_present(), Some(false));
        assert_eq!(missing_scope.relations().len(), 1);
        assert_eq!(
            missing_scope.relations()[0].relation_id.as_ref(),
            "root-object"
        );
        assert!(missing_scope.relations()[0].scope_root);
        assert_eq!(missing_scope.relations()[0].source, source);
        assert_eq!(missing_scope.relations()[0].generation, 1);
        assert!(matches!(
            &missing_scope.relations()[0].state,
            ScopedScopeRelationState::Absent {
                kind: CoverageAbsenceKind::Absent
            }
        ));
        assert!(missing_scope
            .validate_against(host.root_identity(), &missing_watermark.source_coverage,));
        assert!(!format!("{missing_scope:?}").contains(&root.to_string_lossy().to_string()));
        let missing_scope_revision = missing_scope.scope_revision();
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
        let present_scope = &present_watermark.scope_coverage;
        assert_eq!(present_scope.root_present(), Some(true));
        assert!(matches!(
            &present_scope.relations()[0].state,
            ScopedScopeRelationState::Present {
                status: CoverageStatus::CompleteThrough
            }
        ));
        assert_ne!(present_scope.scope_revision(), missing_scope_revision);
        assert!(present_scope
            .validate_against(host.root_identity(), &present_watermark.source_coverage,));
        assert!(!missing_scope
            .validate_against(host.root_identity(), &present_watermark.source_coverage,));
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
        let deleted_scope = &deleted_watermark.scope_coverage;
        assert_eq!(deleted_scope.root_present(), Some(false));
        assert!(matches!(
            &deleted_scope.relations()[0].state,
            ScopedScopeRelationState::Absent {
                kind: CoverageAbsenceKind::Deleted
            }
        ));
        assert_ne!(deleted_scope.scope_revision(), missing_scope_revision);
        assert!(deleted_scope
            .validate_against(host.root_identity(), &deleted_watermark.source_coverage,));
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
    fn scoped_decode_uses_one_declared_dependency_pass_and_common_revision() {
        let registry = declared_dependency_fixture_registry(None);
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("declared-dependency-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        std::fs::write(root.join("sidecar.json"), b"sidecar").unwrap();
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            declared_dependency_scoped_access_request(root),
        )
        .unwrap();
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"declared-dependency-session",
        }];
        let root_origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let dependency_origin = RecordOrigin {
            object_id: 5,
            ..root_origin.clone()
        };
        let requests = [
            ScopedObservationAppendPassRequest {
                relation_id: "root-object",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &root_origin,
                force_contract_replay: false,
            },
            ScopedObservationAppendPassRequest {
                relation_id: "decoder-sidecar",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &dependency_origin,
                force_contract_replay: false,
            },
        ];
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
                    origin: &root_origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();

        assert!(matches!(
            host.decode_append_with_dependencies_for_test(
                &mut object,
                &observation,
                &pass,
                &requests[..1],
                AccessPhase::Initial,
            ),
            Err(ScopedObservationAccessError::InvalidGrant(_))
        ));
        let ScopedAppendDecodeOutcome::Ready(decoded) = host
            .decode_append_with_dependencies_for_test(
                &mut object,
                &observation,
                &pass,
                &requests,
                AccessPhase::Initial,
            )
            .unwrap()
        else {
            panic!("the exact declared dependency must decode");
        };
        let report = pass.report();
        let dependency_report = report
            .relations()
            .iter()
            .find(|relation| relation.relation_id == "decoder-sidecar")
            .unwrap();
        assert_eq!(dependency_report.attempts, 2);
        assert_eq!(dependency_report.bytes_read, 14);
        assert_eq!(dependency_report.trace[0].phase, AccessPhase::Initial);
        assert_eq!(dependency_report.trace[1].phase, AccessPhase::Revalidation);

        let mut admission = admission_lane(16, 64, 4);
        if let Err(failure) = admission.admit(&mut object, &observation, decoded) {
            panic!("declared dependency admission failed: {}", failure.error);
        }
        let mut expected_state = b"one".to_vec();
        expected_state.extend_from_slice(Revision::digest(b"sidecar").as_bytes());
        assert_eq!(object.decoder_state(), Some(expected_state.as_slice()));
        assert_eq!(object.checkpoint().unwrap().committed_offset, 4);
        let diagnostic = format!("{report:?}");
        assert!(!diagnostic.contains("sidecar.json"));
        assert!(!diagnostic.contains("declared-dependency-root"));
    }

    #[test]
    fn scoped_dependency_change_before_state_staging_retries_without_advancing() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("changing-dependency-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        let sidecar = root.join("sidecar.json");
        std::fs::write(&sidecar, b"before").unwrap();
        let registry = declared_dependency_fixture_registry(Some((sidecar, b"after".to_vec())));
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            declared_dependency_scoped_access_request(root),
        )
        .unwrap();
        let mut object = scoped_append_object(
            AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap(),
            RawRetentionPolicy::None,
        );
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"changing-dependency-session",
        }];
        let root_origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let dependency_origin = RecordOrigin {
            object_id: 5,
            ..root_origin.clone()
        };
        let requests = [
            ScopedObservationAppendPassRequest {
                relation_id: "root-object",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &root_origin,
                force_contract_replay: false,
            },
            ScopedObservationAppendPassRequest {
                relation_id: "decoder-sidecar",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &dependency_origin,
                force_contract_replay: false,
            },
        ];

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
                    origin: &root_origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();
        assert!(matches!(
            host.decode_append_with_dependencies_for_test(
                &mut object,
                &observation,
                &pass,
                &requests,
                AccessPhase::Initial,
            ),
            Err(ScopedObservationAccessError::Decode(
                ScopedDecodeFailureClass::Transient
            ))
        ));
        assert!(object.checkpoint().is_none());
        assert!(object.decoder_state().is_none());
        object.discard(&observation).unwrap();
        drop(pass);

        let retry = host.begin_pass().unwrap();
        let retry_observation = object
            .reconcile(
                &retry,
                ScopedAppendReconcileRequest {
                    relation_id: "root-object",
                    identity_inputs: &identity,
                    access_phase: AccessPhase::Revalidation,
                    parent_token: None,
                    depth: 1,
                    max_bytes: 64,
                    origin: &root_origin,
                    force_contract_replay: false,
                },
            )
            .unwrap();
        let ScopedAppendDecodeOutcome::Ready(decoded) = host
            .decode_append_with_dependencies_for_test(
                &mut object,
                &retry_observation,
                &retry,
                &requests,
                AccessPhase::Revalidation,
            )
            .unwrap()
        else {
            panic!("the stable dependency retry must decode");
        };
        let mut admission = admission_lane(16, 64, 4);
        if let Err(failure) = admission.admit(&mut object, &retry_observation, decoded) {
            panic!(
                "stable dependency retry admission failed: {}",
                failure.error
            );
        }
        let mut expected_state = b"one".to_vec();
        expected_state.extend_from_slice(Revision::digest(b"after").as_bytes());
        assert_eq!(object.decoder_state(), Some(expected_state.as_slice()));
        assert_eq!(object.checkpoint().unwrap().committed_offset, 4);
    }

    #[test]
    fn scoped_live_poll_composes_declared_dependencies_without_a_bypass() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("live-dependency-root");
        std::fs::create_dir_all(&root).unwrap();
        let sidecar = root.join("sidecar.json");
        let registry =
            declared_dependency_fixture_registry(Some((sidecar.clone(), b"after\n".to_vec())));
        let host = ScopedObservationAccessHost::authorize(
            &registry,
            declared_dependency_scoped_access_request(root.clone()),
        )
        .unwrap();
        let mut drain = host
            .open_consumer_drain(ScopedObservationDeliveryLimits {
                max_semantic_events: 32,
                max_retained_native_bytes: 128,
                max_source_control_items: 16,
            })
            .unwrap();
        let mut objects = vec![
            scoped_append_object_for_native_object(b"session.jsonl"),
            scoped_append_object_for_native_object(b"sidecar.json"),
        ];
        let mut admission = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events: 32,
            max_retained_native_bytes: 128,
            max_control_items: 16,
            max_coverage_objects: 2,
        })
        .unwrap();
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 16,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"live-dependency-session",
        }];
        let root_origin = RecordOrigin {
            source_instance_id: 13,
            stream_id: 14,
            object_id: 15,
            observed_at: 16,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let dependency_origin = RecordOrigin {
            object_id: 17,
            ..root_origin.clone()
        };

        let bootstrap_ticket = host.request_poll().unwrap();
        let bootstrap = host.begin_poll().unwrap().unwrap();
        reconcile_missing_relation_poll(
            &host,
            &bootstrap,
            "root-object",
            &mut objects[0],
            &mut admission,
            &identity,
            &root_origin,
        );
        reconcile_missing_relation_poll(
            &host,
            &bootstrap,
            "decoder-sidecar",
            &mut objects[1],
            &mut admission,
            &identity,
            &dependency_origin,
        );
        host.complete_bootstrap_poll(bootstrap, &admission, &projection, &drain)
            .unwrap();
        assert!(matches!(
            host.poll_resolution(&bootstrap_ticket).unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));
        for object in &mut objects {
            object.complete_bootstrap().unwrap();
        }
        host.offer_consumer_bootstrap_complete(&objects, &admission, &projection, &mut drain, 18)
            .unwrap();
        let mut active = host
            .bind_consumer_bootstrap_epoch_state(objects, admission, projection, &drain)
            .unwrap();

        std::fs::write(root.join("session.jsonl"), b"one\n").unwrap();
        std::fs::write(&sidecar, b"before\n").unwrap();
        let requests = [
            ScopedObservationAppendPassRequest {
                relation_id: "root-object",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &root_origin,
                force_contract_replay: false,
            },
            ScopedObservationAppendPassRequest {
                relation_id: "decoder-sidecar",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &dependency_origin,
                force_contract_replay: false,
            },
        ];
        let ticket = host.request_poll().unwrap();
        let forged_dependency_origin = RecordOrigin {
            source_instance_id: 99,
            ..dependency_origin.clone()
        };
        let forged_requests = [
            requests[0],
            ScopedObservationAppendPassRequest {
                origin: &forged_dependency_origin,
                ..requests[1]
            },
        ];
        let lease = host.begin_poll().unwrap().unwrap();
        assert!(matches!(
            host.execute_epoch_poll_pass(lease, &mut active, &mut drain, &forged_requests),
            Err(ScopedObservationPassExecutionError::InvalidRelationSet)
        ));
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"before\n");
        assert!(matches!(
            host.poll_resolution(&ticket).unwrap(),
            ScopedObservationPollResolution::Pending
        ));

        let lease = host.begin_poll().unwrap().unwrap();
        assert!(matches!(
            host.execute_epoch_poll_pass(lease, &mut active, &mut drain, &requests),
            Err(ScopedObservationPassExecutionError::Access(
                ScopedObservationAccessError::Decode(ScopedDecodeFailureClass::Transient)
            ))
        ));
        assert!(matches!(
            host.poll_resolution(&ticket).unwrap(),
            ScopedObservationPollResolution::Pending
        ));
        for relation_id in ["root-object", "decoder-sidecar"] {
            let object = active.append_object(relation_id).unwrap();
            assert!(object.checkpoint().is_none());
            assert!(object.decoder_state().is_none());
        }

        let retry = host.begin_poll().unwrap().unwrap();
        host.execute_epoch_poll_pass(retry, &mut active, &mut drain, &requests)
            .unwrap();
        assert!(matches!(
            host.poll_resolution(&ticket).unwrap(),
            ScopedObservationPollResolution::Ready(_)
        ));
        let dependency_revision = Revision::digest(b"after\n");
        for (relation_id, payload, committed_offset) in [
            ("root-object", b"one".as_slice(), 4),
            ("decoder-sidecar", b"after".as_slice(), 6),
        ] {
            let object = active.append_object(relation_id).unwrap();
            let mut expected_state = payload.to_vec();
            expected_state.extend_from_slice(dependency_revision.as_bytes());
            assert_eq!(object.decoder_state(), Some(expected_state.as_slice()));
            assert_eq!(
                object.checkpoint().unwrap().committed_offset,
                committed_offset
            );
        }
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
        assert_eq!(barrier.scope_coverage.root_present(), Some(false));
        assert!(barrier
            .scope_coverage
            .validate_against(host.root_identity(), &barrier.source_coverage));
        assert_eq!(barrier.family_manifest.len(), 1);
        assert_eq!(barrier.family_manifest[0].fact_family, "runtime.usage-v2");
        assert_eq!(barrier.family_manifest[0].entity_or_event_count, 0);
        assert_eq!(&barrier.observation_capabilities, host.capabilities());
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
        other_request.source_instance.spec.stable_key =
            SourceInstanceKey::new(b"fixture-other-source-instance".to_vec()).unwrap();
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
            Err(ScopedReplacementStageError::ResyncStartNotDelivered)
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
        assert_eq!(resync_barrier.scope_coverage, barrier.scope_coverage);
        assert_eq!(resync_barrier.family_manifest, barrier.family_manifest);
        assert_eq!(
            resync_barrier.observation_capabilities,
            barrier.observation_capabilities
        );
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

        // observer.resync_started has not crossed the delivered boundary.
        // Completion cannot retire its strict context or activate any of the
        // three staged state components.
        assert_eq!(
            host.offer_scope_resync_complete(&mut active, &mut stage, &mut delivery, 80,),
            Err(ScopedReplacementStageError::ResyncStartNotDelivered)
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

    #[tokio::test]
    async fn rfc012_d2_observer_composes_claude_root_current_future_and_sidecar() {
        let registry = claude_composed_fixture_registry();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("claude-composed-root");
        std::fs::create_dir_all(&root).unwrap();

        let host = ScopedObservationAccessHost::authorize(
            &registry,
            claude_composed_scoped_access_request(root.clone()),
        )
        .unwrap();
        let mut runtime = ScopedObservationAsyncRuntime::open(
            host,
            ScopedObservationDeliveryLimits {
                max_semantic_events: 16,
                max_retained_native_bytes: 0,
                max_source_control_items: 16,
            },
        )
        .unwrap();
        let handle = runtime.handle();
        let mut objects = vec![
            scoped_append_object_for_native_object(b"session.jsonl"),
            scoped_append_object_for_native_object(b"agent-current.jsonl"),
            scoped_append_object_for_native_object(b"agent-future.jsonl"),
            scoped_append_object_for_native_object(b"team-inbox.json"),
        ];
        let identities: Vec<_> = objects
            .iter()
            .map(|object| object.source_identity().clone())
            .collect();
        assert_eq!(identities.len(), 4);
        assert_ne!(identities[0], identities[1]);
        assert_ne!(identities[0], identities[2]);
        assert_ne!(identities[0], identities[3]);
        assert_ne!(identities[1], identities[2]);
        assert_ne!(identities[1], identities[3]);
        assert_ne!(identities[2], identities[3]);

        let mut admission = admission_lane_for_objects(4);
        let projection = ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 16,
        })
        .unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"claude-composed-session",
        }];
        let root_origin = RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let current_origin = RecordOrigin {
            object_id: 31,
            ..root_origin.clone()
        };
        let future_origin = RecordOrigin {
            object_id: 32,
            ..root_origin.clone()
        };
        let sidecar_origin = RecordOrigin {
            object_id: 33,
            ..root_origin.clone()
        };

        let bootstrap_ticket = handle.host().request_poll().unwrap();
        let bootstrap_lease = handle.host().begin_poll().unwrap().unwrap();
        let root_obs = reconcile_named_relation(
            handle.host(),
            bootstrap_lease.access_pass(),
            &mut objects[0],
            &mut admission,
            named_relation_request(
                "root-transcript",
                &identity,
                &root_origin,
                AccessPhase::Initial,
            ),
        );
        let current_obs = reconcile_named_relation(
            handle.host(),
            bootstrap_lease.access_pass(),
            &mut objects[1],
            &mut admission,
            named_relation_request(
                "current-child",
                &identity,
                &current_origin,
                AccessPhase::Initial,
            ),
        );
        let future_obs = reconcile_named_relation(
            handle.host(),
            bootstrap_lease.access_pass(),
            &mut objects[2],
            &mut admission,
            named_relation_request(
                "future-child",
                &identity,
                &future_origin,
                AccessPhase::Initial,
            ),
        );
        let sidecar_obs = reconcile_named_relation(
            handle.host(),
            bootstrap_lease.access_pass(),
            &mut objects[3],
            &mut admission,
            named_relation_request(
                "team-inbox-sidecar",
                &identity,
                &sidecar_origin,
                AccessPhase::Initial,
            ),
        );
        assert!(!root_obs.object_present);
        assert!(!current_obs.object_present);
        assert!(
            !future_obs.object_present,
            "future child must attach before the native object exists"
        );
        assert!(!sidecar_obs.object_present);
        assert_eq!(objects[0].relation_id(), Some("root-transcript"));
        assert_eq!(objects[1].relation_id(), Some("current-child"));
        assert_eq!(objects[2].relation_id(), Some("future-child"));
        assert_eq!(objects[3].relation_id(), Some("team-inbox-sidecar"));

        let bootstrap_watermark = handle.with_attachment(|host, drain| {
            host.complete_bootstrap_poll(bootstrap_lease, &admission, &projection, drain)
                .unwrap()
        });
        assert!(matches!(
            bootstrap_ticket.wait_async().await,
            ScopedObservationPollResolution::Ready(watermark)
                if Arc::ptr_eq(&watermark, &bootstrap_watermark)
        ));
        for object in &mut objects {
            object.complete_bootstrap().unwrap();
        }
        let barrier = handle.with_attachment(|host, drain| {
            host.offer_consumer_bootstrap_complete(&objects, &admission, &projection, drain, 50)
                .unwrap()
        });
        let bootstrap = runtime.next_event().await.unwrap().unwrap();
        assert!(matches!(
            &bootstrap.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { barrier: delivered }
                if Arc::ptr_eq(delivered, &barrier)
        ));
        runtime
            .acknowledge_applied(bootstrap.application_receipt())
            .unwrap();

        let active = handle
            .with_attachment(|host, drain| {
                host.bind_consumer_bootstrap_epoch_state(objects, admission, projection, drain)
            })
            .unwrap();
        let bindings = vec![
            ScopedObservationAppendPassBinding::new(
                "root-transcript",
                vec![ScopedObservationOwnedIdentityInput::new(
                    "native-session-id",
                    b"claude-composed-session".to_vec(),
                )
                .unwrap()],
                None,
                1,
                64,
                root_origin,
                false,
            )
            .unwrap(),
            ScopedObservationAppendPassBinding::new(
                "current-child",
                vec![ScopedObservationOwnedIdentityInput::new(
                    "native-session-id",
                    b"claude-composed-session".to_vec(),
                )
                .unwrap()],
                None,
                1,
                64,
                current_origin,
                false,
            )
            .unwrap(),
            ScopedObservationAppendPassBinding::new(
                "future-child",
                vec![ScopedObservationOwnedIdentityInput::new(
                    "native-session-id",
                    b"claude-composed-session".to_vec(),
                )
                .unwrap()],
                None,
                1,
                64,
                future_origin,
                false,
            )
            .unwrap(),
            ScopedObservationAppendPassBinding::new(
                "team-inbox-sidecar",
                vec![ScopedObservationOwnedIdentityInput::new(
                    "native-session-id",
                    b"claude-composed-session".to_vec(),
                )
                .unwrap()],
                None,
                1,
                64,
                sidecar_origin,
                false,
            )
            .unwrap(),
        ];
        let owner = handle
            .bind_epoch_source_owner(
                active,
                bindings,
                ScopedObservationSourceOwnerRetryPolicy::default(),
            )
            .unwrap();
        let source_task = tokio::spawn(owner.run_until_stopped_with_clock(|| 100));
        std::fs::write(root.join("session.jsonl"), b"root\n").unwrap();
        std::fs::write(root.join("agent-current.jsonl"), b"current-child\n").unwrap();
        std::fs::write(root.join("team-inbox.json"), b"sidecar\n").unwrap();
        let current = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(current, ScopedObservationPollResolution::Ready(_)));
        std::fs::write(root.join("agent-future.jsonl"), b"future-child\n").unwrap();
        let created = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(created, ScopedObservationPollResolution::Ready(_)));

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), source_task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped.exit(),
            ScopedObservationSourceOwnerRunExit::Cancelled
        ));
        assert!(close.wait_async().await.complete);
    }

    #[tokio::test]
    async fn rfc012_d3_shared_pass_pool_serializes_catalog_like_work_and_observer_pass() {
        let pool = SharedSourcePassPool::new(1).unwrap();
        assert_eq!(pool.max_concurrent_passes(), 1);
        assert_eq!(pool.available_permits(), 1);

        let catalog_held = Arc::new(tokio::sync::Notify::new());
        let release_catalog = Arc::new(tokio::sync::Notify::new());
        let catalog_task = {
            let pool = pool.clone();
            let catalog_held = Arc::clone(&catalog_held);
            let release_catalog = Arc::clone(&release_catalog);
            tokio::spawn(async move {
                let permit = pool.acquire_for_test().await;
                catalog_held.notify_one();
                release_catalog.notified().await;
                drop(permit);
                "catalog-retained-page"
            })
        };
        catalog_held.notified().await;
        assert_eq!(pool.available_permits(), 0);

        let durable_started = Instant::now();
        let durable_wait = tokio::time::timeout(
            std::time::Duration::from_millis(40),
            pool.acquire_for_test(),
        )
        .await;
        assert!(
            durable_wait.is_err(),
            "durable/catalog work must occupy the shared permit until it releases"
        );
        assert!(durable_started.elapsed() >= std::time::Duration::from_millis(30));

        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let watcher_policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            1,
        )
        .unwrap();
        let (runtime, handle, pair, target, drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("d3-fair-root"),
                    b"d3-fair-session".to_vec(),
                ),
                b"d3-fair-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(
                    ScopedObservationSourceOwnerRetryPolicy::default(),
                    watcher_policy,
                )
                .with_pass_pool(pool.clone()),
            )
            .await;
        let owner = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 200, || 201));
        std::fs::write(&target, b"observer\n").unwrap();
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(50), handle.poll()).await;
        assert!(
            blocked.is_err(),
            "observer source passes must wait on the same catalog/durable permit"
        );

        release_catalog.notify_one();
        let catalog = tokio::time::timeout(std::time::Duration::from_secs(2), catalog_task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(catalog, "catalog-retained-page");
        let ready = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ready, ScopedObservationPollResolution::Ready(_)));

        let close = runtime.request_close();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stopped,
            ScopedObservationAsyncOwnerRunResult::Stopped(_)
        ));
        assert!(close.wait_async().await.complete);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rfc012_d5_emit_observer_kernel_report() {
        use std::time::Instant;

        fn sample(started: Instant) -> (u64, u64) {
            let elapsed = started.elapsed();
            (
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            )
        }

        let registry = stateful_supported_fixture_registry();
        let temp = TempDir::new().unwrap();
        let watcher_policy = || {
            ScopedObservationNativeWatcherRecoveryPolicy::new(
                std::time::Duration::from_secs(60),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                1,
            )
            .unwrap()
        };
        let source_policy = ScopedObservationSourceOwnerRetryPolicy::default();

        let attach_started = Instant::now();
        let (runtime, handle, pair, target, drops, _) =
            automatic_single_object_pair_with_root_and_watcher_policy(
                &registry,
                AutomaticSingleObjectFixtureRoot::distinct(
                    temp.path().join("d5-attach-root"),
                    b"d5-attach-session".to_vec(),
                ),
                b"d5-attach-session".to_vec(),
                AppendDelimitedConfig::json_lines(),
                128,
                AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy()),
            )
            .await;
        let (attach_ms, attach_us) = sample(attach_started);

        let owner = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 100, || 101));
        std::fs::write(&target, b"d5\n").unwrap();
        let poll_started = Instant::now();
        let poll = tokio::time::timeout(std::time::Duration::from_secs(2), handle.poll())
            .await
            .unwrap()
            .unwrap();
        let (poll_ms, poll_us) = sample(poll_started);
        assert!(matches!(poll, ScopedObservationPollResolution::Ready(_)));

        let overflow_started = Instant::now();
        let required = handle
            .require_resync(ScopedResyncReason::WatcherOverflow, 110)
            .unwrap();
        let overflow_result = tokio::time::timeout(std::time::Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap();
        let (overflow_ms, overflow_us) = sample(overflow_started);
        let ScopedObservationAsyncOwnerRunResult::Resync(handoff) = overflow_result else {
            panic!("overflow must retain a replacement handoff");
        };
        drop(required);

        let multi_started = Instant::now();
        let mut scopes = Vec::new();
        for (label, session) in [
            ("d5-scope-a", b"d5-scope-a".as_slice()),
            ("d5-scope-b", b"d5-scope-b".as_slice()),
            ("d5-scope-c", b"d5-scope-c".as_slice()),
        ] {
            scopes.push(
                automatic_single_object_pair_with_root_and_watcher_policy(
                    &registry,
                    AutomaticSingleObjectFixtureRoot::distinct(
                        temp.path().join(label),
                        session.to_vec(),
                    ),
                    session.to_vec(),
                    AppendDelimitedConfig::json_lines(),
                    128,
                    AutomaticSingleObjectOwnerPolicies::new(source_policy, watcher_policy()),
                )
                .await,
            );
        }
        let (multi_scope_ms, multi_scope_us) = sample(multi_started);
        assert_eq!(scopes.len(), 3);

        let report = serde_json::json!({
            "source_test": "rfc012_d5_emit_observer_kernel_report",
            "package": "D5",
            "gate": "experiment-not-ratified-ceiling",
            "operations": [
                { "label": "attach", "t_ms": attach_ms, "t_us": attach_us },
                { "label": "poll", "t_ms": poll_ms, "t_us": poll_us },
                { "label": "overflow-resync", "t_ms": overflow_ms, "t_us": overflow_us },
                { "label": "three-scope-attach", "t_ms": multi_scope_ms, "t_us": multi_scope_us, "scopes": 3 }
            ]
        });
        if let Some(path) = std::env::var_os("RFC012_D5_REPORT").map(std::path::PathBuf::from) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }

        let close = runtime.request_close();
        drop(handoff);
        assert!(close.wait_async().await.complete);
        for (scope_runtime, _, pair, _, scope_drops, _) in scopes {
            let task = tokio::spawn(pair.run_with_factory_and_clocks(|_| Err(()), || 200, || 201));
            let close = scope_runtime.request_close();
            let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                stopped,
                ScopedObservationAsyncOwnerRunResult::Stopped(_)
            ));
            assert!(close.wait_async().await.complete);
            assert_eq!(scope_drops.load(Ordering::SeqCst), 1);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
