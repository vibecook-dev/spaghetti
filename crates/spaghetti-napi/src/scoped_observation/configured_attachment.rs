//! Store-free configured-root composition for one RFC 012D attachment.
//!
//! Contract negotiation and the registered bounded native probe happen before
//! adapter discovery. Discovery may identify native roots, but it cannot mint
//! source authority: every retained root, known locator, identity input, and
//! runtime stream is re-bound to the selected promoted scope program before
//! the one-shot attachment host is constructed.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::adapter::{
    AdapterRegistry, CanonicalEntityKey, CoverageDomain, DiscoveryContext, DriverSpec,
    ExternalEntityRef, FactSemanticContext, NativeIdentityClaim, ScopeJoinEvidence,
    ScopeJoinParameterSet, ScopeRelationBounds, ScopeRelationPrimitive, SourceInstance,
    SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor, StreamSpec,
};
use crate::observation_contract::{ObservationContractOffer, ObservationContractRequest};
use crate::source::{
    confined_relative_path_key, platform_path_key, validate_relation_id, AccessPhase,
    AppendDelimitedFile, AppendRead, AuthorizedScopeAccessPlan, RecordOrigin, ScopeIdentityInput,
    SourceMediaType, MAX_IDENTITY_VALUE_BYTES,
};

use super::{
    artifact_access, prepare_scoped_observation_support, PreparedScopedObservationSupport,
    ScopedAccessRootGrant, ScopedAppendDecodeOutcome, ScopedAppendDecoderConfig,
    ScopedAppendDeliveryPhase, ScopedAppendReconcileRequest, ScopedArtifactAccessPolicy,
    ScopedArtifactRelationGrant, ScopedBootstrapBarrierError, ScopedDeliveryError,
    ScopedKnownAppendObject, ScopedKnownObjectGrant, ScopedObjectFailureClassification,
    ScopedObservationAccessError, ScopedObservationAccessHost, ScopedObservationAccessPass,
    ScopedObservationAdmissionLane, ScopedObservationAppendPassBinding,
    ScopedObservationAppendPassRequest, ScopedObservationAsyncHandle,
    ScopedObservationAsyncOwnerRunResult, ScopedObservationAsyncResyncFailure,
    ScopedObservationAsyncRuntime, ScopedObservationAsyncStoppedOwners,
    ScopedObservationConsumerOfferError, ScopedObservationDeliveryLimits,
    ScopedObservationDirectoryPassBinding, ScopedObservationNativeWatchBackend,
    ScopedObservationNativeWatchCallback, ScopedObservationNativeWatcher,
    ScopedObservationNativeWatcherRecoveryPolicy, ScopedObservationOpenDrainError,
    ScopedObservationOwnedIdentityInput, ScopedObservationProjectionLimits,
    ScopedObservationProjectionSink, ScopedObservationQueueLimits,
    ScopedObservationScopeJoinSnapshot, ScopedObservationSourceOwnerRetryPolicy,
    ScopedObservationStartupError, ScopedObservationStartupReconcileAction,
    ScopedObservationTrustedAccessRequest, ScopedObservationUnknownWireNegotiation,
    ScopedObserverFailureReason, ScopedProjectionDeliveryError, ScopedRootIdentityRequest,
    ScopedSourceObjectErrorRuntime, ScopedSourceObjectFailureCode, SCOPED_INITIAL_SCOPE_EPOCH,
};

const MAX_CONFIGURED_ROOTS: usize = 16;
const MAX_DISCOVERED_SOURCE_INSTANCES: usize = 64;
const MAX_CONFIGURED_ROOT_KEY_BYTES: usize = 32 * 1024;
const MAX_CONFIGURED_ROOT_KEY_BYTES_TOTAL: usize = 256 * 1024;
const MAX_RELATION_IDENTITY_INPUTS: usize = 32;
const MAX_KNOWN_OBJECTS: usize = 64;

/// Pre-discovery identity supplied by the trusted host. Values remain private
/// and are converted to declaration-ordered inputs only after a promoted
/// scope program has been selected.
#[derive(Clone)]
pub(crate) struct ScopedConfiguredRootIdentity {
    session_identity_key: Arc<[u8]>,
    root_run_identity_key: Option<Arc<[u8]>>,
    relation_identity_inputs: BTreeMap<String, Arc<[u8]>>,
    expected_session_key: Option<CanonicalEntityKey>,
    external_session_ref: Option<ExternalEntityRef>,
    native_session_claim: Option<NativeIdentityClaim>,
}

impl ScopedConfiguredRootIdentity {
    pub(crate) fn new(
        session_identity_key: impl Into<Arc<[u8]>>,
        relation_identity_inputs: BTreeMap<String, Arc<[u8]>>,
    ) -> Result<Self, ScopedObservationAccessError> {
        let value = Self {
            session_identity_key: session_identity_key.into(),
            root_run_identity_key: None,
            relation_identity_inputs,
            expected_session_key: None,
            external_session_ref: None,
            native_session_claim: None,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn with_root_run_identity_key(mut self, value: Arc<[u8]>) -> Self {
        self.root_run_identity_key = Some(value);
        self
    }

    pub(crate) fn with_optional_root_run_identity_key(mut self, value: Option<Arc<[u8]>>) -> Self {
        self.root_run_identity_key = value;
        self
    }

    pub(crate) fn with_expected_session(
        mut self,
        key: CanonicalEntityKey,
        external: ExternalEntityRef,
    ) -> Self {
        self.expected_session_key = Some(key);
        self.external_session_ref = Some(external);
        self
    }

    pub(crate) fn with_native_session_claim(mut self, claim: NativeIdentityClaim) -> Self {
        self.native_session_claim = Some(claim);
        self
    }

    fn validate(&self) -> Result<(), ScopedObservationAccessError> {
        if self.session_identity_key.is_empty()
            || self.session_identity_key.len() > MAX_IDENTITY_VALUE_BYTES
            || self
                .root_run_identity_key
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > MAX_IDENTITY_VALUE_BYTES)
            || self.relation_identity_inputs.is_empty()
            || self.relation_identity_inputs.len() > MAX_RELATION_IDENTITY_INPUTS
            || self.relation_identity_inputs.iter().any(|(name, value)| {
                validate_relation_id(name).is_err()
                    || value.is_empty()
                    || value.len() > MAX_IDENTITY_VALUE_BYTES
            })
            || self
                .expected_session_key
                .is_some_and(|key| self.external_session_ref != Some(ExternalEntityRef::new(key)))
            || self.external_session_ref.is_some() != self.expected_session_key.is_some()
        {
            return Err(invalid_configured_attachment());
        }
        Ok(())
    }

    fn root_request(&self, spec: &SourceInstanceSpec) -> ScopedRootIdentityRequest {
        let request = ScopedRootIdentityRequest::new(
            spec.identity_contract_version,
            spec.stable_key.as_bytes().to_vec(),
            Arc::clone(&self.session_identity_key),
            self.root_run_identity_key.clone(),
            self.expected_session_key,
            self.external_session_ref,
        );
        match &self.native_session_claim {
            Some(claim) => request.with_native_session_claim(claim.clone()),
            None => request,
        }
    }

    fn ordered_inputs(
        &self,
        names: &[String],
    ) -> Result<Vec<ScopedObservationOwnedIdentityInput>, ScopedObservationAccessError> {
        names
            .iter()
            .map(|name| {
                let value = self
                    .relation_identity_inputs
                    .get(name)
                    .ok_or_else(invalid_configured_attachment)?;
                ScopedObservationOwnedIdentityInput::new(name.clone(), value.as_ref().to_vec())
                    .map_err(|_| invalid_configured_attachment())
            })
            .collect()
    }
}

impl std::fmt::Debug for ScopedConfiguredRootIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedConfiguredRootIdentity")
            .field(
                "relation_identity_input_names",
                &self
                    .relation_identity_inputs
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .field(
                "has_root_run_identity",
                &self.root_run_identity_key.is_some(),
            )
            .field("has_expected_session", &self.expected_session_key.is_some())
            .field(
                "has_native_session_claim",
                &self.native_session_claim.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// Trusted, bounded configured-root request. Native paths and identity values
/// are intentionally absent from Debug output and never become portable DTOs.
#[derive(Clone)]
pub(crate) struct ScopedConfiguredAttachmentRequest {
    adapter_id: String,
    configured_roots: Vec<PathBuf>,
    program_id: String,
    known_object_relative_paths: BTreeMap<String, PathBuf>,
    identity: ScopedConfiguredRootIdentity,
    artifact_access_policy: ScopedArtifactAccessPolicy,
    artifact_relations: Vec<ScopedArtifactRelationGrant>,
    observation_contract_request: ObservationContractRequest,
    observation_contract_offer: ObservationContractOffer,
    unknown_wire_contract: Option<ScopedObservationUnknownWireNegotiation>,
}

impl ScopedConfiguredAttachmentRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        adapter_id: impl Into<String>,
        configured_roots: Vec<PathBuf>,
        program_id: impl Into<String>,
        known_object_relative_paths: BTreeMap<String, PathBuf>,
        identity: ScopedConfiguredRootIdentity,
        observation_contract_request: ObservationContractRequest,
        observation_contract_offer: ObservationContractOffer,
    ) -> Result<Self, ScopedObservationAccessError> {
        let value = Self {
            adapter_id: adapter_id.into(),
            configured_roots,
            program_id: program_id.into(),
            known_object_relative_paths,
            identity,
            artifact_access_policy: ScopedArtifactAccessPolicy::disabled(),
            artifact_relations: Vec::new(),
            observation_contract_request,
            observation_contract_offer,
            unknown_wire_contract: None,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn with_unknown_wire_contract(
        mut self,
        contract: ScopedObservationUnknownWireNegotiation,
    ) -> Self {
        self.unknown_wire_contract = Some(contract);
        self
    }

    pub(crate) fn with_artifact_access(
        mut self,
        policy: ScopedArtifactAccessPolicy,
        relations: Vec<ScopedArtifactRelationGrant>,
    ) -> Self {
        self.artifact_access_policy = policy;
        self.artifact_relations = relations;
        self
    }

    fn validate(&self) -> Result<(), ScopedObservationAccessError> {
        self.identity.validate()?;
        if validate_relation_id(&self.adapter_id).is_err()
            || validate_relation_id(&self.program_id).is_err()
            || self.configured_roots.is_empty()
            || self.configured_roots.len() > MAX_CONFIGURED_ROOTS
            || self.known_object_relative_paths.is_empty()
            || self.known_object_relative_paths.len() > MAX_KNOWN_OBJECTS
        {
            return Err(invalid_configured_attachment());
        }
        let mut root_keys = BTreeSet::new();
        let mut total_root_key_bytes = 0_usize;
        for root in &self.configured_roots {
            if root.as_os_str().is_empty() || !root.is_absolute() {
                return Err(invalid_configured_attachment());
            }
            let key = platform_path_key(root);
            if key.len() > MAX_CONFIGURED_ROOT_KEY_BYTES
                || !root_keys.insert(key.clone())
                || total_root_key_bytes
                    .checked_add(key.len())
                    .is_none_or(|total| total > MAX_CONFIGURED_ROOT_KEY_BYTES_TOTAL)
            {
                return Err(invalid_configured_attachment());
            }
            total_root_key_bytes += key.len();
        }
        if self
            .known_object_relative_paths
            .iter()
            .any(|(relation, path)| {
                validate_relation_id(relation).is_err()
                    || match confined_relative_path_key(path) {
                        Ok(key) => key.len() <= 1 || key.len() > MAX_IDENTITY_VALUE_BYTES,
                        Err(_) => true,
                    }
            })
        {
            return Err(invalid_configured_attachment());
        }
        Ok(())
    }
}

impl std::fmt::Debug for ScopedConfiguredAttachmentRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedConfiguredAttachmentRequest")
            .field("adapter_id", &self.adapter_id)
            .field("program_id", &self.program_id)
            .field("configured_root_count", &self.configured_roots.len())
            .field(
                "known_object_relation_ids",
                &self
                    .known_object_relative_paths
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .field("identity", &self.identity)
            .field("artifact_access_policy", &self.artifact_access_policy)
            .field("artifact_relation_count", &self.artifact_relations.len())
            .field(
                "has_unknown_wire_contract",
                &self.unknown_wire_contract.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// One exact known object bound to its promoted stream declaration. The
/// native root and locator stay inside the host; this value retains only the
/// relation name and declarative runtime stream needed by the source owner.
#[derive(Clone)]
pub(crate) struct PreparedScopedKnownObjectSource {
    relation_id: String,
    stream: StreamSpec,
    relative_path: PathBuf,
    max_bytes: u64,
}

impl PreparedScopedKnownObjectSource {
    pub(crate) fn relation_id(&self) -> &str {
        &self.relation_id
    }

    pub(crate) fn stream(&self) -> &StreamSpec {
        &self.stream
    }
}

impl std::fmt::Debug for PreparedScopedKnownObjectSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedScopedKnownObjectSource")
            .field("relation_id", &self.relation_id)
            .field("stream_id", &self.stream.id)
            .field("relative_path", &"<redacted>")
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

/// Decoder-ready append sources derived entirely from one composed attachment.
/// No native object has been opened or watched yet. Numeric `RecordOrigin`
/// coordinates are attachment-local registration coordinates; semantic source
/// identity is derived only from the adapter/source/stream/object contract.
pub(crate) struct PreparedConfiguredAppendRuntime {
    host: ScopedObservationAccessHost,
    objects: Vec<ScopedKnownAppendObject>,
    bindings: Vec<ScopedObservationAppendPassBinding>,
    directory_bindings: Vec<PreparedScopedDirectoryRelationBinding>,
    related_relation_bindings: Vec<PreparedScopedRelatedRelationBinding>,
    required_coverage_objects: usize,
}

/// Declaration-owned coordinates for one configured child-directory source.
/// This value retains no rendered locator or native path. The access host must
/// still mint a fresh pass-local source binding before every scan.
#[derive(Clone)]
pub(crate) struct PreparedScopedDirectoryRelationBinding {
    relation_id: String,
    identity_inputs: Vec<ScopedObservationOwnedIdentityInput>,
    bounds: ScopeRelationBounds,
}

impl PreparedScopedDirectoryRelationBinding {
    pub(crate) fn relation_id(&self) -> &str {
        &self.relation_id
    }

    pub(crate) fn identity_input_names(&self) -> impl Iterator<Item = &str> {
        self.identity_inputs
            .iter()
            .map(ScopedObservationOwnedIdentityInput::name)
    }

    pub(crate) fn bounds(&self) -> ScopeRelationBounds {
        self.bounds
    }

    fn borrowed_identity_inputs(&self) -> Vec<ScopeIdentityInput<'_>> {
        self.identity_inputs
            .iter()
            .map(|input| ScopeIdentityInput {
                name: &input.name,
                value: &input.value,
            })
            .collect()
    }

    fn owner_binding(
        &self,
    ) -> Result<ScopedObservationDirectoryPassBinding, ConfiguredScopedObservationRuntimeError>
    {
        ScopedObservationDirectoryPassBinding::new(
            self.relation_id.clone(),
            self.identity_inputs.clone(),
        )
        .map_err(|_| ConfiguredScopedObservationRuntimeError::SourceBinding)
    }
}

impl std::fmt::Debug for PreparedScopedDirectoryRelationBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedScopedDirectoryRelationBinding")
            .field("relation_id", &self.relation_id)
            .field(
                "identity_input_names",
                &self.identity_input_names().collect::<Vec<_>>(),
            )
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

/// Declaration-owned shape for one evidence-derived exact-object relation.
///
/// Unlike known objects and configured child directories, the identity values
/// for these relations must come from retained adapter join evidence. This
/// value therefore carries only the exact declared input names and bounds; it
/// cannot render a locator or reserve native access by itself.
#[derive(Clone)]
pub(crate) struct PreparedScopedRelatedRelationBinding {
    relation_id: String,
    primitive: ScopeRelationPrimitive,
    identity_input_names: Vec<String>,
    bounds: ScopeRelationBounds,
}

impl PreparedScopedRelatedRelationBinding {
    pub(crate) fn relation_id(&self) -> &str {
        &self.relation_id
    }

    pub(crate) fn primitive(&self) -> ScopeRelationPrimitive {
        self.primitive
    }

    pub(crate) fn identity_input_names(&self) -> impl Iterator<Item = &str> {
        self.identity_input_names.iter().map(String::as_str)
    }

    pub(crate) fn bounds(&self) -> ScopeRelationBounds {
        self.bounds
    }
}

impl std::fmt::Debug for PreparedScopedRelatedRelationBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedScopedRelatedRelationBinding")
            .field("relation_id", &self.relation_id)
            .field("primitive", &self.primitive)
            .field("identity_input_names", &self.identity_input_names)
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

/// One exact evidence-derived identity set matched to its promoted relation
/// declaration. It is still pre-I/O: the common access pass must independently
/// render, reserve, and bind the runtime stream before any source is opened.
#[derive(Clone)]
pub(crate) struct PreparedScopedRelatedSourceBinding {
    relation_id: String,
    primitive: ScopeRelationPrimitive,
    parameter: ScopeJoinParameterSet,
    evidence_groups: Vec<Vec<ScopeJoinEvidence>>,
    bounds: ScopeRelationBounds,
}

impl PreparedScopedRelatedSourceBinding {
    pub(crate) fn relation_id(&self) -> &str {
        &self.relation_id
    }

    pub(crate) fn primitive(&self) -> ScopeRelationPrimitive {
        self.primitive
    }

    pub(crate) fn identity_input_names(&self) -> impl Iterator<Item = &str> {
        self.parameter
            .identity_inputs()
            .iter()
            .map(|input| input.name())
    }

    pub(crate) fn evidence_group_count(&self) -> usize {
        self.evidence_groups.len()
    }

    pub(crate) fn bounds(&self) -> ScopeRelationBounds {
        self.bounds
    }

    pub(crate) fn borrowed_identity_inputs(&self) -> Vec<ScopeIdentityInput<'_>> {
        self.parameter
            .identity_inputs()
            .iter()
            .map(|input| ScopeIdentityInput {
                name: input.name(),
                value: input.value(),
            })
            .collect()
    }
}

impl std::fmt::Debug for PreparedScopedRelatedSourceBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedScopedRelatedSourceBinding")
            .field("relation_id", &self.relation_id)
            .field("primitive", &self.primitive)
            .field(
                "identity_input_names",
                &self.identity_input_names().collect::<Vec<_>>(),
            )
            .field("evidence_group_count", &self.evidence_groups.len())
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

/// Deterministic pre-I/O reconciliation plan for the current retained join
/// snapshot. Equal native coordinates from independent evidence owners are
/// read once while preserving every owner group for later lifecycle proof.
pub(crate) struct PreparedScopedRelatedReconciliationPlan {
    declared_relation_ids: BTreeSet<String>,
    sources: Vec<PreparedScopedRelatedSourceBinding>,
    snapshot_retained_bytes: usize,
}

impl PreparedScopedRelatedReconciliationPlan {
    pub(crate) fn sources(&self) -> &[PreparedScopedRelatedSourceBinding] {
        &self.sources
    }

    pub(crate) fn declared_relation_ids(&self) -> impl Iterator<Item = &str> {
        self.declared_relation_ids.iter().map(String::as_str)
    }

    pub(crate) fn snapshot_retained_bytes(&self) -> usize {
        self.snapshot_retained_bytes
    }
}

impl std::fmt::Debug for PreparedScopedRelatedReconciliationPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedScopedRelatedReconciliationPlan")
            .field("declared_relation_ids", &self.declared_relation_ids)
            .field("source_count", &self.sources.len())
            .field("snapshot_retained_bytes", &self.snapshot_retained_bytes)
            .finish_non_exhaustive()
    }
}

/// Fixed internal resource policy for the first configured observer owner.
/// These are correctness bounds rather than performance claims; a public
/// request contract may narrow them later, but cannot widen native authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfiguredScopedObservationRuntimeOptions {
    admission: ScopedObservationQueueLimits,
    delivery: ScopedObservationDeliveryLimits,
    projection: ScopedObservationProjectionLimits,
    watcher_hint_capacity: usize,
    startup_reconcile_hint_limit: usize,
    max_startup_reconcile_passes: usize,
    source_retry: ScopedObservationSourceOwnerRetryPolicy,
    watcher_recovery: ScopedObservationNativeWatcherRecoveryPolicy,
}

impl Default for ConfiguredScopedObservationRuntimeOptions {
    fn default() -> Self {
        Self {
            admission: ScopedObservationQueueLimits {
                max_data_events: 4_096,
                max_retained_native_bytes: 4 * 1_024 * 1_024,
                max_control_items: 512,
                max_coverage_objects: MAX_KNOWN_OBJECTS,
            },
            delivery: ScopedObservationDeliveryLimits {
                max_semantic_events: 1_024,
                max_retained_native_bytes: 4 * 1_024 * 1_024,
                max_source_control_items: 256,
            },
            projection: ScopedObservationProjectionLimits {
                max_usage_v2_entities: 4_096,
            },
            watcher_hint_capacity: 256,
            startup_reconcile_hint_limit: 64,
            max_startup_reconcile_passes: 256,
            source_retry: ScopedObservationSourceOwnerRetryPolicy::default(),
            watcher_recovery: ScopedObservationNativeWatcherRecoveryPolicy::default(),
        }
    }
}

impl ConfiguredScopedObservationRuntimeOptions {
    fn validate(
        self,
        object_count: usize,
        required_coverage_objects: usize,
    ) -> Result<Self, ConfiguredScopedObservationRuntimeError> {
        if object_count == 0
            || object_count > MAX_KNOWN_OBJECTS
            || required_coverage_objects < object_count
            || self.admission.max_coverage_objects < required_coverage_objects
            || self.watcher_hint_capacity == 0
            || self.watcher_hint_capacity > 4_096
            || self.startup_reconcile_hint_limit == 0
            || self.startup_reconcile_hint_limit > 4_096
            || self.max_startup_reconcile_passes == 0
            || self.max_startup_reconcile_passes > 4_096
        {
            return Err(ConfiguredScopedObservationRuntimeError::InvalidOptions);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ConfiguredScopedObservationRuntimeError {
    #[error("configured scoped observer options are invalid")]
    InvalidOptions,
    #[error("configured scoped observer admission could not be created")]
    AdmissionOpen,
    #[error("configured scoped observer projection could not be created")]
    ProjectionOpen,
    #[error("configured scoped observer event drain could not be created")]
    RuntimeOpen,
    #[error("configured scoped observer watcher could not be installed")]
    WatcherInstall,
    #[error("configured scoped observer bootstrap ordering failed")]
    Startup,
    #[error("configured scoped observer source or decoder pass failed")]
    SourcePass,
    #[error("configured scoped observer admission failed")]
    Admission,
    #[error("configured scoped observer delivery failed")]
    Delivery,
    #[error("configured scoped observer bootstrap exceeded its bounded reconciliation ceiling")]
    ReconcileLimit,
    #[error("configured scoped observer epoch could not be bound")]
    EpochBinding,
    #[error("configured scoped observer source owner could not be bound")]
    SourceBinding,
    #[error("configured scoped observer watcher/source owners could not be paired")]
    OwnerPairBinding,
    #[error("configured scoped observer was closed")]
    Closed,
}

/// The non-cloneable event owner and its structured producer supervisor. No
/// task is detached here: callers must drive both futures concurrently and
/// retain the returned runtime until close completes.
pub(crate) struct OpenedConfiguredAppendRuntime {
    runtime: ScopedObservationAsyncRuntime,
    handle: ScopedObservationAsyncHandle,
    supervisor: ConfiguredScopedObservationSupervisor,
}

impl OpenedConfiguredAppendRuntime {
    pub(crate) fn into_parts(
        self,
    ) -> (
        ScopedObservationAsyncRuntime,
        ScopedObservationAsyncHandle,
        ConfiguredScopedObservationSupervisor,
    ) {
        (self.runtime, self.handle, self.supervisor)
    }
}

impl std::fmt::Debug for OpenedConfiguredAppendRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenedConfiguredAppendRuntime")
            .field("adapter_id", &self.handle.host().root_identity().adapter_id)
            .field(
                "session_key",
                &self.handle.host().root_identity().session_key,
            )
            .field("supervisor", &self.supervisor)
            .finish_non_exhaustive()
    }
}

#[must_use = "the configured observer supervisor must be driven beside the event owner"]
pub(crate) struct ConfiguredScopedObservationSupervisor {
    handle: ScopedObservationAsyncHandle,
    watcher: ScopedObservationNativeWatcher,
    objects: Vec<ScopedKnownAppendObject>,
    bindings: Vec<ScopedObservationAppendPassBinding>,
    directory_bindings: Vec<PreparedScopedDirectoryRelationBinding>,
    admission: ScopedObservationAdmissionLane,
    projection: ScopedObservationProjectionSink,
    bootstrap_object_errors: BTreeMap<String, ScopedSourceObjectErrorRuntime>,
    options: ConfiguredScopedObservationRuntimeOptions,
}

impl std::fmt::Debug for ConfiguredScopedObservationSupervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfiguredScopedObservationSupervisor")
            .field("adapter_id", &self.handle.host().root_identity().adapter_id)
            .field(
                "session_key",
                &self.handle.host().root_identity().session_key,
            )
            .field("watcher", &self.watcher)
            .field(
                "relation_ids",
                &self
                    .bindings
                    .iter()
                    .map(ScopedObservationAppendPassBinding::relation_id)
                    .collect::<Vec<_>>(),
            )
            .field(
                "directory_relation_ids",
                &self
                    .directory_bindings
                    .iter()
                    .map(PreparedScopedDirectoryRelationBinding::relation_id)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

#[must_use = "a configured supervisor result retains terminal owner state or failure evidence"]
pub(crate) enum ConfiguredScopedObservationSupervisorRunResult {
    Stopped(Box<ScopedObservationAsyncStoppedOwners>),
    BootstrapFailed(ConfiguredScopedObservationRuntimeError),
    ResyncFailed(ScopedObservationAsyncResyncFailure),
}

impl std::fmt::Debug for ConfiguredScopedObservationSupervisorRunResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped(stopped) => formatter.debug_tuple("Stopped").field(stopped).finish(),
            Self::BootstrapFailed(error) => formatter
                .debug_tuple("BootstrapFailed")
                .field(error)
                .finish(),
            Self::ResyncFailed(error) => {
                formatter.debug_tuple("ResyncFailed").field(error).finish()
            }
        }
    }
}

impl PreparedConfiguredAppendRuntime {
    pub(crate) fn host(&self) -> &ScopedObservationAccessHost {
        &self.host
    }

    pub(crate) fn objects(&self) -> &[ScopedKnownAppendObject] {
        &self.objects
    }

    pub(crate) fn bindings(&self) -> &[ScopedObservationAppendPassBinding] {
        &self.bindings
    }

    pub(crate) fn directory_bindings(&self) -> &[PreparedScopedDirectoryRelationBinding] {
        &self.directory_bindings
    }

    pub(crate) fn related_relation_bindings(&self) -> &[PreparedScopedRelatedRelationBinding] {
        &self.related_relation_bindings
    }

    /// Match a committed reducer snapshot to the exact related relations in
    /// this attachment. This performs no locator rendering and creates no
    /// access reservation. Unknown relation IDs, wrong input order, and the
    /// first owner/source beyond declaration bounds fail closed.
    pub(crate) fn plan_related_sources(
        &self,
        snapshot: &ScopedObservationScopeJoinSnapshot,
    ) -> Result<PreparedScopedRelatedReconciliationPlan, ConfiguredScopedObservationRuntimeError>
    {
        struct Candidate<'snapshot> {
            definition: &'snapshot PreparedScopedRelatedRelationBinding,
            parameter: &'snapshot ScopeJoinParameterSet,
            evidence: &'snapshot [ScopeJoinEvidence],
        }

        let definitions = self
            .related_relation_bindings
            .iter()
            .map(|binding| (binding.relation_id.as_str(), binding))
            .collect::<BTreeMap<_, _>>();
        let declared_relation_ids = definitions
            .keys()
            .map(|relation_id| (*relation_id).to_string())
            .collect::<BTreeSet<_>>();
        let directory_definitions = self
            .directory_bindings
            .iter()
            .map(|binding| (binding.relation_id.as_str(), binding))
            .collect::<BTreeMap<_, _>>();
        let mut candidates = Vec::new();
        for entry in snapshot.entries() {
            let Some(definition) = definitions.get(entry.relation_id()).copied() else {
                // A configured directory can carry corroborating adapter join
                // evidence, but it cannot replace the already-bound root
                // coordinate. Anything else is undeclared or the wrong
                // primitive for this join channel.
                let Some(directory) = directory_definitions.get(entry.relation_id()).copied()
                else {
                    return Err(ConfiguredScopedObservationRuntimeError::SourceBinding);
                };
                if entry.parameters().iter().any(|parameter| {
                    parameter.identity_inputs().len() != directory.identity_inputs.len()
                        || parameter
                            .identity_inputs()
                            .iter()
                            .zip(&directory.identity_inputs)
                            .any(|(actual, expected)| {
                                actual.name() != expected.name || actual.value() != expected.value
                            })
                }) {
                    return Err(ConfiguredScopedObservationRuntimeError::SourceBinding);
                }
                continue;
            };
            let max_fan_out = usize::try_from(definition.bounds.max_fan_out)
                .map_err(|_| ConfiguredScopedObservationRuntimeError::SourceBinding)?;
            if entry.parameters().len() > max_fan_out {
                return Err(ConfiguredScopedObservationRuntimeError::SourceBinding);
            }
            for parameter in entry.parameters() {
                if parameter.identity_inputs().len() != definition.identity_input_names.len()
                    || parameter
                        .identity_inputs()
                        .iter()
                        .zip(&definition.identity_input_names)
                        .any(|(actual, expected)| actual.name() != expected)
                {
                    return Err(ConfiguredScopedObservationRuntimeError::SourceBinding);
                }
                candidates.push(Candidate {
                    definition,
                    parameter,
                    evidence: entry.evidence(),
                });
            }
        }
        candidates.sort_by(|left, right| {
            left.definition
                .relation_id
                .cmp(&right.definition.relation_id)
                .then_with(|| {
                    left.parameter
                        .identity_inputs()
                        .iter()
                        .map(|input| (input.name(), input.value()))
                        .cmp(
                            right
                                .parameter
                                .identity_inputs()
                                .iter()
                                .map(|input| (input.name(), input.value())),
                        )
                })
        });

        let mut relation_source_counts = BTreeMap::<&str, usize>::new();
        let mut sources = Vec::<PreparedScopedRelatedSourceBinding>::new();
        for candidate in candidates {
            if let Some(existing) = sources.last_mut().filter(|source| {
                source.relation_id == candidate.definition.relation_id
                    && source.parameter == *candidate.parameter
            }) {
                existing.evidence_groups.push(candidate.evidence.to_vec());
                continue;
            }
            let source_count = relation_source_counts
                .entry(candidate.definition.relation_id.as_str())
                .or_default();
            *source_count = source_count
                .checked_add(1)
                .ok_or(ConfiguredScopedObservationRuntimeError::SourceBinding)?;
            let max_objects = usize::try_from(candidate.definition.bounds.max_objects)
                .map_err(|_| ConfiguredScopedObservationRuntimeError::SourceBinding)?;
            if *source_count > max_objects {
                return Err(ConfiguredScopedObservationRuntimeError::SourceBinding);
            }
            sources.push(PreparedScopedRelatedSourceBinding {
                relation_id: candidate.definition.relation_id.clone(),
                primitive: candidate.definition.primitive,
                parameter: candidate.parameter.clone(),
                evidence_groups: vec![candidate.evidence.to_vec()],
                bounds: candidate.definition.bounds,
            });
        }
        Ok(PreparedScopedRelatedReconciliationPlan {
            declared_relation_ids,
            sources,
            snapshot_retained_bytes: snapshot.retained_bytes(),
        })
    }

    pub(crate) fn required_coverage_objects(&self) -> usize {
        self.required_coverage_objects
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ScopedObservationAccessHost,
        Vec<ScopedKnownAppendObject>,
        Vec<ScopedObservationAppendPassBinding>,
        Vec<PreparedScopedDirectoryRelationBinding>,
        Vec<PreparedScopedRelatedRelationBinding>,
    ) {
        (
            self.host,
            self.objects,
            self.bindings,
            self.directory_bindings,
            self.related_relation_bindings,
        )
    }

    /// Create the sole event drain and install the native watcher without
    /// reading any source object. The returned supervisor performs the scan;
    /// callers drive it concurrently with the non-cloneable event runtime.
    pub(crate) fn open(
        self,
        options: ConfiguredScopedObservationRuntimeOptions,
    ) -> Result<OpenedConfiguredAppendRuntime, ConfiguredScopedObservationRuntimeError> {
        self.open_with_watcher_factory_inner(options, |callback| {
            let watcher = notify::recommended_watcher(callback).map_err(|_| ())?;
            Ok(Box::new(watcher))
        })
    }

    fn open_with_watcher_factory_inner<F>(
        self,
        options: ConfiguredScopedObservationRuntimeOptions,
        factory: F,
    ) -> Result<OpenedConfiguredAppendRuntime, ConfiguredScopedObservationRuntimeError>
    where
        F: FnOnce(
            ScopedObservationNativeWatchCallback,
        ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>,
    {
        // Related-object relations need a retained join snapshot and a fresh
        // pass-local reservation for every rendered locator. Do not let a
        // partially composed runtime silently omit them while that owner is
        // being installed.
        if !self.related_relation_bindings.is_empty() {
            return Err(ConfiguredScopedObservationRuntimeError::SourceBinding);
        }
        let options = options.validate(self.objects.len(), self.required_coverage_objects)?;
        let admission = ScopedObservationAdmissionLane::new(options.admission)
            .map_err(|_| ConfiguredScopedObservationRuntimeError::AdmissionOpen)?;
        let projection = self
            .host
            .open_projection_sink(options.projection)
            .map_err(|_| ConfiguredScopedObservationRuntimeError::ProjectionOpen)?;
        let runtime = ScopedObservationAsyncRuntime::open(self.host, options.delivery).map_err(
            |_: ScopedObservationOpenDrainError| {
                ConfiguredScopedObservationRuntimeError::RuntimeOpen
            },
        )?;
        let handle = runtime.handle();
        let watcher = handle
            .install_native_watcher_with_factory_inner(options.watcher_hint_capacity, factory)
            .map_err(|_| ConfiguredScopedObservationRuntimeError::WatcherInstall)?;
        let supervisor = ConfiguredScopedObservationSupervisor {
            handle: handle.clone(),
            watcher,
            objects: self.objects,
            bindings: self.bindings,
            directory_bindings: self.directory_bindings,
            admission,
            projection,
            bootstrap_object_errors: BTreeMap::new(),
            options,
        };
        Ok(OpenedConfiguredAppendRuntime {
            runtime,
            handle,
            supervisor,
        })
    }

    #[cfg(test)]
    pub(crate) fn open_with_watcher_factory<F>(
        self,
        options: ConfiguredScopedObservationRuntimeOptions,
        factory: F,
    ) -> Result<OpenedConfiguredAppendRuntime, ConfiguredScopedObservationRuntimeError>
    where
        F: FnOnce(
            ScopedObservationNativeWatchCallback,
        ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>,
    {
        self.open_with_watcher_factory_inner(options, factory)
    }

    #[cfg(test)]
    pub(crate) fn with_forced_contract_replay_for_test(mut self) -> Self {
        for binding in &mut self.bindings {
            binding.force_contract_replay = true;
        }
        self
    }
}

impl std::fmt::Debug for PreparedConfiguredAppendRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedConfiguredAppendRuntime")
            .field("adapter_id", &self.host.root_identity().adapter_id)
            .field("session_key", &self.host.root_identity().session_key)
            .field("object_count", &self.objects.len())
            .field("required_coverage_objects", &self.required_coverage_objects)
            .field(
                "relation_ids",
                &self
                    .bindings
                    .iter()
                    .map(ScopedObservationAppendPassBinding::relation_id)
                    .collect::<Vec<_>>(),
            )
            .field(
                "directory_relation_ids",
                &self
                    .directory_bindings
                    .iter()
                    .map(PreparedScopedDirectoryRelationBinding::relation_id)
                    .collect::<Vec<_>>(),
            )
            .field(
                "related_relation_ids",
                &self
                    .related_relation_bindings
                    .iter()
                    .map(PreparedScopedRelatedRelationBinding::relation_id)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl ConfiguredScopedObservationSupervisor {
    /// Drive bootstrap, then supervise the inseparable watcher/source pair
    /// through any number of whole-epoch resync replacements until close or a
    /// terminal failure. This future owns all producer-side mutable state.
    pub(crate) async fn run_until_stopped(
        mut self,
    ) -> ConfiguredScopedObservationSupervisorRunResult {
        if let Err(error) = self.bootstrap().await {
            configured_fail_watcher(&mut self.watcher);
            return ConfiguredScopedObservationSupervisorRunResult::BootstrapFailed(error);
        }

        // Contract replay is a one-shot migration instruction. Bootstrap has
        // now drained every exact object through its completion boundary, so
        // carrying the bit into the live source owner would restart later
        // polls at offset zero and manufacture a new generation forever.
        for binding in &mut self.bindings {
            binding.force_contract_replay = false;
        }

        let Self {
            handle,
            mut watcher,
            objects,
            bindings,
            directory_bindings,
            admission,
            projection,
            bootstrap_object_errors,
            options,
        } = self;
        let active = match handle.with_attachment(move |host, drain| {
            host.bind_consumer_bootstrap_epoch_state_with_errors(
                objects,
                admission,
                projection,
                bootstrap_object_errors,
                drain,
            )
        }) {
            Ok(active) => active,
            Err(_error) => {
                configured_fail_watcher(&mut watcher);
                return ConfiguredScopedObservationSupervisorRunResult::BootstrapFailed(
                    ConfiguredScopedObservationRuntimeError::EpochBinding,
                );
            }
        };
        let owner_directory_bindings = match directory_bindings
            .iter()
            .map(PreparedScopedDirectoryRelationBinding::owner_binding)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(bindings) => bindings,
            Err(error) => {
                configured_fail_watcher(&mut watcher);
                return ConfiguredScopedObservationSupervisorRunResult::BootstrapFailed(error);
            }
        };
        let source = match handle.bind_epoch_source_owner_with_directories(
            active,
            bindings,
            owner_directory_bindings,
            options.source_retry,
        ) {
            Ok(source) => source,
            Err(failure) => {
                let (_error, active, bindings, directory_bindings) = failure.into_parts();
                drop((active, bindings, directory_bindings));
                configured_fail_watcher(&mut watcher);
                return ConfiguredScopedObservationSupervisorRunResult::BootstrapFailed(
                    ConfiguredScopedObservationRuntimeError::SourceBinding,
                );
            }
        };
        let mut pair = match handle.bind_live_owner_pair(watcher, source, options.watcher_recovery)
        {
            Ok(pair) => pair,
            Err(failure) => {
                let (_error, mut watcher, source, _policy) = failure.into_parts();
                drop(source);
                configured_fail_watcher(&mut watcher);
                return ConfiguredScopedObservationSupervisorRunResult::BootstrapFailed(
                    ConfiguredScopedObservationRuntimeError::OwnerPairBinding,
                );
            }
        };

        loop {
            match pair.run_until_stopped().await {
                ScopedObservationAsyncOwnerRunResult::Stopped(stopped) => {
                    return ConfiguredScopedObservationSupervisorRunResult::Stopped(Box::new(
                        stopped,
                    ));
                }
                ScopedObservationAsyncOwnerRunResult::Resync(handoff) => {
                    pair = match handoff.replay_and_rebind().await {
                        Ok(pair) => pair,
                        Err(error) => {
                            return ConfiguredScopedObservationSupervisorRunResult::ResyncFailed(
                                error,
                            );
                        }
                    };
                }
            }
        }
    }

    async fn bootstrap(&mut self) -> Result<(), ConfiguredScopedObservationRuntimeError> {
        let scan = self
            .watcher
            .coordinator()
            .begin_initial_scan(self.handle.host())
            .map_err(|_| ConfiguredScopedObservationRuntimeError::Startup)?;
        configured_execute_append_pass(
            &self.handle,
            scan.access_pass(),
            &mut self.objects,
            &self.bindings,
            &mut self.admission,
            &mut self.projection,
            &mut self.bootstrap_object_errors,
            self.options.source_retry,
            AccessPhase::Initial,
            configured_observed_at(),
        )
        .await?;
        configured_execute_directory_pass(
            &self.handle,
            scan.access_pass(),
            &self.directory_bindings,
            &mut self.admission,
            &mut self.projection,
            AccessPhase::Initial,
            configured_observed_at(),
        )
        .await?;
        self.handle
            .with_attachment(|host, drain| {
                self.watcher.coordinator().finish_initial_scan(
                    host,
                    scan,
                    &self.admission,
                    &self.projection,
                    drain,
                )
            })
            .map_err(|_| ConfiguredScopedObservationRuntimeError::Startup)?;

        let mut completed_reconcile_passes = 0_usize;
        loop {
            if self.objects.iter().any(|object| object.bootstrap_blocked) {
                self.watcher
                    .request_audit()
                    .map_err(|_| ConfiguredScopedObservationRuntimeError::Startup)?;
            }

            loop {
                match self
                    .watcher
                    .coordinator()
                    .next_reconcile(
                        self.handle.host(),
                        self.options.startup_reconcile_hint_limit,
                    )
                    .map_err(|_| ConfiguredScopedObservationRuntimeError::Startup)?
                {
                    ScopedObservationStartupReconcileAction::CaughtUp => break,
                    ScopedObservationStartupReconcileAction::Reconcile(reconcile) => {
                        completed_reconcile_passes = completed_reconcile_passes
                            .checked_add(1)
                            .ok_or(ConfiguredScopedObservationRuntimeError::ReconcileLimit)?;
                        if completed_reconcile_passes > self.options.max_startup_reconcile_passes {
                            return Err(ConfiguredScopedObservationRuntimeError::ReconcileLimit);
                        }
                        configured_execute_append_pass(
                            &self.handle,
                            reconcile.access_pass(),
                            &mut self.objects,
                            &self.bindings,
                            &mut self.admission,
                            &mut self.projection,
                            &mut self.bootstrap_object_errors,
                            self.options.source_retry,
                            AccessPhase::Revalidation,
                            configured_observed_at(),
                        )
                        .await?;
                        configured_execute_directory_pass(
                            &self.handle,
                            reconcile.access_pass(),
                            &self.directory_bindings,
                            &mut self.admission,
                            &mut self.projection,
                            AccessPhase::Revalidation,
                            configured_observed_at(),
                        )
                        .await?;
                        self.handle
                            .with_attachment(|host, drain| {
                                self.watcher.coordinator().finish_reconcile(
                                    host,
                                    reconcile,
                                    &self.admission,
                                    &self.projection,
                                    drain,
                                )
                            })
                            .map_err(|_| ConfiguredScopedObservationRuntimeError::Startup)?;
                    }
                }
            }

            let (waiter, generation, offered) = self.handle.with_attachment(|host, drain| {
                let waiter = drain.delivery_capacity_waiter();
                let generation = waiter.snapshot().generation;
                let offered = self.watcher.coordinator().complete_and_offer_bootstrap(
                    host,
                    &mut self.objects,
                    &self.admission,
                    &self.projection,
                    drain,
                    configured_observed_at(),
                );
                (waiter, generation, offered)
            });
            match offered {
                Ok(_) => return Ok(()),
                Err(ScopedObservationStartupError::ReconcilePending { .. }) => continue,
                Err(error) if configured_startup_is_backpressure(&error) => {
                    if waiter.wait_after_async(generation).await.closed {
                        return Err(ConfiguredScopedObservationRuntimeError::Closed);
                    }
                }
                Err(ScopedObservationStartupError::Closed) => {
                    return Err(ConfiguredScopedObservationRuntimeError::Closed);
                }
                Err(_) => return Err(ConfiguredScopedObservationRuntimeError::Startup),
            }
        }
    }
}

fn configured_observed_at() -> i64 {
    super::scoped_observation_now_unix_ms()
}

fn configured_fail_watcher(watcher: &mut ScopedObservationNativeWatcher) {
    let _ = watcher.fail_observer(
        ScopedObserverFailureReason::InternalControlFailure,
        configured_observed_at(),
    );
}

async fn configured_execute_directory_pass(
    handle: &ScopedObservationAsyncHandle,
    pass: &ScopedObservationAccessPass,
    bindings: &[PreparedScopedDirectoryRelationBinding],
    admission: &mut ScopedObservationAdmissionLane,
    projection: &mut ScopedObservationProjectionSink,
    phase: AccessPhase,
    observed_at: i64,
) -> Result<(), ConfiguredScopedObservationRuntimeError> {
    let delivery_phase = match phase {
        AccessPhase::Initial => ScopedAppendDeliveryPhase::Bootstrap,
        AccessPhase::Revalidation => ScopedAppendDeliveryPhase::Correction,
    };
    for (relation_index, binding) in bindings.iter().enumerate() {
        if !admission.is_empty() {
            return Err(ConfiguredScopedObservationRuntimeError::Admission);
        }
        let previous = admission.directory_relation_listing(binding.relation_id());
        let relation_ordinal = u64::try_from(relation_index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or(ConfiguredScopedObservationRuntimeError::SourcePass)?;
        let owner_binding = binding.owner_binding()?;
        let batch = super::scoped_read_directory_relation_snapshot(
            handle.host(),
            pass,
            &owner_binding,
            super::ScopedObservationDirectorySnapshotRequest {
                previous,
                prior_admission: admission,
                phase,
                relation_ordinal,
                observed_at,
            },
        )
        .map_err(|_| ConfiguredScopedObservationRuntimeError::SourcePass)?;
        admission
            .record_relation_membership(pass.pass_id(), delivery_phase, batch.membership)
            .map_err(|_| ConfiguredScopedObservationRuntimeError::Admission)?;
        configured_offer_pending(handle, admission, projection).await?;

        for lifecycle in batch.members {
            admission
                .admit_directory_member(pass.pass_id(), delivery_phase, lifecycle)
                .map_err(|_| ConfiguredScopedObservationRuntimeError::Admission)?;
            configured_offer_pending(handle, admission, projection).await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn configured_execute_append_pass(
    handle: &ScopedObservationAsyncHandle,
    pass: &ScopedObservationAccessPass,
    objects: &mut [ScopedKnownAppendObject],
    bindings: &[ScopedObservationAppendPassBinding],
    admission: &mut ScopedObservationAdmissionLane,
    projection: &mut ScopedObservationProjectionSink,
    object_errors: &mut BTreeMap<String, ScopedSourceObjectErrorRuntime>,
    retry_policy: ScopedObservationSourceOwnerRetryPolicy,
    phase: AccessPhase,
    observed_at: i64,
) -> Result<(), ConfiguredScopedObservationRuntimeError> {
    if objects.len() != bindings.len() || objects.is_empty() {
        return Err(ConfiguredScopedObservationRuntimeError::SourcePass);
    }
    let identity_inputs = bindings
        .iter()
        .map(|binding| {
            binding
                .identity_inputs
                .iter()
                .map(|input| ScopeIdentityInput {
                    name: &input.name,
                    value: &input.value,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let origins = bindings
        .iter()
        .map(|binding| {
            let mut origin = binding.origin.clone();
            origin.observed_at = observed_at;
            origin
        })
        .collect::<Vec<_>>();
    let requests = bindings
        .iter()
        .zip(&identity_inputs)
        .zip(&origins)
        .map(
            |((binding, identity_inputs), origin)| ScopedObservationAppendPassRequest {
                relation_id: &binding.relation_id,
                identity_inputs,
                parent_token: binding.parent_token,
                depth: binding.depth,
                max_bytes: binding.max_bytes,
                origin,
                force_contract_replay: binding.force_contract_replay,
            },
        )
        .collect::<Vec<_>>();

    for index in 0..objects.len() {
        let request = &requests[index];
        let observation = match objects[index].reconcile(
            pass,
            ScopedAppendReconcileRequest {
                relation_id: request.relation_id,
                identity_inputs: request.identity_inputs,
                access_phase: phase,
                parent_token: request.parent_token,
                depth: request.depth,
                max_bytes: request.max_bytes,
                origin: request.origin,
                force_contract_replay: request.force_contract_replay,
            },
        ) {
            Ok(observation) => observation,
            Err(error) => {
                let error = super::ScopedObservationPassExecutionError::Access(error);
                let Some(classification) = super::scoped_object_failure_classification(&error)
                else {
                    return Err(ConfiguredScopedObservationRuntimeError::SourcePass);
                };
                configured_record_bootstrap_object_error(
                    &mut objects[index],
                    request.relation_id,
                    pass.pass_id(),
                    admission,
                    object_errors,
                    retry_policy,
                    classification,
                    observed_at,
                )?;
                continue;
            }
        };
        if matches!(observation.read, AppendRead::RetryTransient) {
            objects[index]
                .discard(&observation)
                .map_err(|_| ConfiguredScopedObservationRuntimeError::SourcePass)?;
            configured_record_bootstrap_object_error(
                &mut objects[index],
                request.relation_id,
                pass.pass_id(),
                admission,
                object_errors,
                retry_policy,
                ScopedObjectFailureClassification::Retryable(
                    ScopedSourceObjectFailureCode::SourceRetryTransient,
                ),
                observed_at,
            )?;
            continue;
        }
        let decoded = match handle.host().decode_append_with_dependencies(
            &mut objects[index],
            &observation,
            pass,
            &requests,
            phase,
        ) {
            Ok(ScopedAppendDecodeOutcome::Ready(decoded)) => decoded,
            Ok(ScopedAppendDecodeOutcome::RetryTransient) => {
                objects[index]
                    .discard(&observation)
                    .map_err(|_| ConfiguredScopedObservationRuntimeError::SourcePass)?;
                configured_record_bootstrap_object_error(
                    &mut objects[index],
                    request.relation_id,
                    pass.pass_id(),
                    admission,
                    object_errors,
                    retry_policy,
                    ScopedObjectFailureClassification::Retryable(
                        ScopedSourceObjectFailureCode::DecodeRetryTransient,
                    ),
                    observed_at,
                )?;
                continue;
            }
            Err(error) => {
                objects[index]
                    .discard(&observation)
                    .map_err(|_| ConfiguredScopedObservationRuntimeError::SourcePass)?;
                let error = super::ScopedObservationPassExecutionError::Access(error);
                let Some(classification) = super::scoped_object_failure_classification(&error)
                else {
                    return Err(ConfiguredScopedObservationRuntimeError::SourcePass);
                };
                configured_record_bootstrap_object_error(
                    &mut objects[index],
                    request.relation_id,
                    pass.pass_id(),
                    admission,
                    object_errors,
                    retry_policy,
                    classification,
                    observed_at,
                )?;
                continue;
            }
        };
        if let Err(failure) = admission.admit(&mut objects[index], &observation, decoded) {
            objects[index]
                .discard(&observation)
                .map_err(|_| ConfiguredScopedObservationRuntimeError::Admission)?;
            let _error = failure.error;
            let _decoded = failure.decoded;
            return Err(ConfiguredScopedObservationRuntimeError::Admission);
        }
        configured_offer_pending(handle, admission, projection).await?;
        object_errors.remove(request.relation_id);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn configured_record_bootstrap_object_error(
    object: &mut ScopedKnownAppendObject,
    relation_id: &str,
    access_pass_id: u64,
    admission: &mut ScopedObservationAdmissionLane,
    object_errors: &mut BTreeMap<String, ScopedSourceObjectErrorRuntime>,
    retry_policy: ScopedObservationSourceOwnerRetryPolicy,
    classification: ScopedObjectFailureClassification,
    observed_at: i64,
) -> Result<(), ConfiguredScopedObservationRuntimeError> {
    let prior_attempts = object_errors
        .get(relation_id)
        .map_or(0, |state| state.error.retry.failed_attempts());
    let (error, _retry_delay) = super::scoped_prepare_object_error(
        object,
        relation_id,
        SCOPED_INITIAL_SCOPE_EPOCH,
        prior_attempts,
        classification,
        retry_policy,
    )
    .map_err(|_| ConfiguredScopedObservationRuntimeError::SourcePass)?;
    object
        .commit_bootstrap_unavailable()
        .map_err(|_| ConfiguredScopedObservationRuntimeError::SourcePass)?;
    admission
        .bind_known_object_error_membership(object)
        .map_err(|_| ConfiguredScopedObservationRuntimeError::Admission)?;
    admission
        .record_object_error_coverage(object, access_pass_id, &error, true)
        .map_err(|_| ConfiguredScopedObservationRuntimeError::Admission)?;
    object_errors.insert(
        relation_id.to_string(),
        ScopedSourceObjectErrorRuntime {
            error,
            observed_at,
            control_offered: false,
        },
    );
    Ok(())
}

async fn configured_offer_pending(
    handle: &ScopedObservationAsyncHandle,
    admission: &mut ScopedObservationAdmissionLane,
    projection: &mut ScopedObservationProjectionSink,
) -> Result<(), ConfiguredScopedObservationRuntimeError> {
    loop {
        let (waiter, generation, offered) = handle.with_attachment(|host, drain| {
            let waiter = drain.delivery_capacity_waiter();
            let generation = waiter.snapshot().generation;
            let offered = host.offer_consumer_next(admission, projection, drain);
            (waiter, generation, offered)
        });
        match offered {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(()),
            Err(error) if configured_offer_is_backpressure(&error) => {
                if waiter.wait_after_async(generation).await.closed {
                    return Err(ConfiguredScopedObservationRuntimeError::Closed);
                }
            }
            Err(ScopedObservationConsumerOfferError::Closed) => {
                return Err(ConfiguredScopedObservationRuntimeError::Closed);
            }
            Err(_) => return Err(ConfiguredScopedObservationRuntimeError::Delivery),
        }
    }
}

fn configured_offer_is_backpressure(error: &ScopedObservationConsumerOfferError) -> bool {
    matches!(
        error,
        ScopedObservationConsumerOfferError::Offer(ScopedProjectionDeliveryError::Delivery(
            ScopedDeliveryError::SemanticQueueFull
                | ScopedDeliveryError::RetainedNativeQueueFull
                | ScopedDeliveryError::SourceControlQueueFull
        ))
    )
}

fn configured_startup_is_backpressure(error: &ScopedObservationStartupError) -> bool {
    matches!(
        error,
        ScopedObservationStartupError::Bootstrap(ScopedBootstrapBarrierError::Delivery(
            ScopedDeliveryError::SemanticQueueFull
                | ScopedDeliveryError::RetainedNativeQueueFull
                | ScopedDeliveryError::SourceControlQueueFull
        ))
    )
}

/// Fully composed store-free attachment authority. It has not opened a file,
/// installed a watcher, created a delivery drain, or started a pass.
pub(crate) struct PreparedScopedObservationAttachment {
    host: ScopedObservationAccessHost,
    root_relation_id: String,
    known_object_sources: BTreeMap<String, PreparedScopedKnownObjectSource>,
    directory_relation_bounds: BTreeMap<String, ScopeRelationBounds>,
    related_relation_bindings: BTreeMap<String, PreparedScopedRelatedRelationBinding>,
    unsupported_observation_relations: BTreeSet<String>,
    relation_identity_inputs: BTreeMap<String, Vec<ScopedObservationOwnedIdentityInput>>,
}

impl PreparedScopedObservationAttachment {
    pub(crate) fn host(&self) -> &ScopedObservationAccessHost {
        &self.host
    }

    pub(crate) fn root_source(&self) -> &PreparedScopedKnownObjectSource {
        self.known_object_sources
            .get(&self.root_relation_id)
            .expect("a prepared scoped attachment retains its validated root source")
    }

    pub(crate) fn known_object_sources(
        &self,
    ) -> impl Iterator<Item = &PreparedScopedKnownObjectSource> {
        self.known_object_sources.values()
    }

    pub(crate) fn relation_identity_inputs(
        &self,
        relation_id: &str,
    ) -> Option<&[ScopedObservationOwnedIdentityInput]> {
        self.relation_identity_inputs
            .get(relation_id)
            .map(Vec::as_slice)
    }

    pub(crate) fn related_relation_bindings(
        &self,
    ) -> impl Iterator<Item = &PreparedScopedRelatedRelationBinding> {
        self.related_relation_bindings.values()
    }

    /// Bind every exact known object to the common append driver and decoder
    /// before the observer installs a watcher. A non-append stream, adapter
    /// bootstrap failure, or identity inconsistency fails closed without
    /// reading the configured root or echoing native input.
    pub(crate) fn prepare_append_runtime(
        self,
        max_facts_per_record: usize,
        max_diagnostics_per_record: usize,
    ) -> Result<PreparedConfiguredAppendRuntime, ScopedObservationAccessError> {
        if max_facts_per_record == 0 || max_diagnostics_per_record == 0 {
            return Err(invalid_configured_runtime());
        }
        let Self {
            host,
            root_relation_id: _,
            known_object_sources,
            directory_relation_bounds,
            related_relation_bindings,
            unsupported_observation_relations,
            relation_identity_inputs,
        } = self;
        if !unsupported_observation_relations.is_empty()
            || known_object_sources
                .len()
                .checked_add(directory_relation_bounds.len())
                != Some(relation_identity_inputs.len())
        {
            return Err(unsupported_configured_runtime());
        }
        if directory_relation_bounds.keys().any(|relation_id| {
            known_object_sources.contains_key(relation_id)
                || related_relation_bindings.contains_key(relation_id)
        }) || related_relation_bindings
            .keys()
            .any(|relation_id| known_object_sources.contains_key(relation_id))
        {
            return Err(invalid_configured_runtime());
        }

        let adapter_id = host.adapter.manifest().id.clone();
        let source_instance = Arc::clone(&host.source_instance);
        let coverage_domains = host
            .contract_selection()
            .contract_versions
            .fact_family_versions
            .iter()
            .map(|(family, version)| CoverageDomain::FactFamily {
                family: family.clone(),
                version: *version,
            })
            .collect::<Vec<_>>();
        let media_type = SourceMediaType::new("application/x-ndjson")
            .map_err(|_| invalid_configured_runtime())?;
        let mut objects = Vec::with_capacity(known_object_sources.len());
        let mut bindings = Vec::with_capacity(known_object_sources.len());

        for (index, (relation_id, source)) in known_object_sources.into_iter().enumerate() {
            if relation_id != source.relation_id {
                return Err(invalid_configured_runtime());
            }
            let DriverSpec::AppendDelimited(config) = &source.stream.driver else {
                return Err(unsupported_configured_runtime());
            };
            let object_key = confined_relative_path_key(&source.relative_path)
                .map_err(|_| invalid_configured_runtime())?;
            let descriptor = SourceObjectDescriptor {
                stream_id: source.stream.id.clone(),
                object_key: object_key.clone(),
                relative_path: source.relative_path.clone(),
            };
            let object_context = catch_unwind(AssertUnwindSafe(|| {
                host.adapter
                    .bootstrap_object_without_source_access(&source_instance, &descriptor)
            }))
            .map_err(|_| configured_decoder_binding_failed())?
            .map_err(|_| configured_decoder_binding_failed())?;
            let semantic_context = FactSemanticContext::new(
                &adapter_id,
                source_instance.spec.identity_contract_version,
                source_instance.spec.stable_key.as_bytes(),
                source.stream.id.as_str().as_bytes(),
                &object_key,
                source.stream.driver.framing_contract_version(),
            )
            .map_err(|_| invalid_configured_runtime())?;
            let driver = AppendDelimitedFile::new(config.clone())
                .map_err(|_| invalid_configured_runtime())?;
            let object = ScopedKnownAppendObject::new(
                driver,
                ScopedAppendDecoderConfig {
                    decoder: source.stream.decoder.clone(),
                    object_context,
                    semantic_context,
                    coverage_domains: coverage_domains.clone(),
                    retention: source.stream.retention,
                    max_facts_per_record,
                    max_diagnostics_per_record,
                },
            )?;
            let ordinal = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(invalid_configured_runtime)?;
            let origin = RecordOrigin {
                source_instance_id: 1,
                stream_id: ordinal,
                object_id: ordinal,
                observed_at: 0,
                source_timestamp_hint: None,
                media_type: media_type.clone(),
            };
            let identity_inputs = relation_identity_inputs
                .get(&relation_id)
                .cloned()
                .ok_or_else(invalid_configured_runtime)?;
            let binding = ScopedObservationAppendPassBinding::new(
                relation_id,
                identity_inputs,
                None,
                1,
                source.max_bytes,
                origin,
                false,
            )
            .map_err(|_| invalid_configured_runtime())?;
            objects.push(object);
            bindings.push(binding);
        }
        let mut directory_bindings = Vec::with_capacity(directory_relation_bounds.len());
        let mut required_coverage_objects = objects.len();
        for (relation_id, bounds) in directory_relation_bounds {
            let identity_inputs = relation_identity_inputs
                .get(&relation_id)
                .cloned()
                .ok_or_else(invalid_configured_runtime)?;
            let max_members =
                usize::try_from(bounds.max_objects).map_err(|_| invalid_configured_runtime())?;
            required_coverage_objects = required_coverage_objects
                .checked_add(1)
                .and_then(|count| count.checked_add(max_members))
                .ok_or_else(invalid_configured_runtime)?;
            directory_bindings.push(PreparedScopedDirectoryRelationBinding {
                relation_id,
                identity_inputs,
                bounds,
            });
        }
        let mut prepared_related_bindings = Vec::with_capacity(related_relation_bindings.len());
        for (_, binding) in related_relation_bindings {
            let max_objects = usize::try_from(binding.bounds.max_objects)
                .map_err(|_| invalid_configured_runtime())?;
            required_coverage_objects = required_coverage_objects
                .checked_add(max_objects)
                .ok_or_else(invalid_configured_runtime)?;
            prepared_related_bindings.push(binding);
        }
        Ok(PreparedConfiguredAppendRuntime {
            host,
            objects,
            bindings,
            directory_bindings,
            related_relation_bindings: prepared_related_bindings,
            required_coverage_objects,
        })
    }

    pub(crate) fn into_host(self) -> ScopedObservationAccessHost {
        self.host
    }
}

impl std::fmt::Debug for PreparedScopedObservationAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedScopedObservationAttachment")
            .field("root_relation_id", &self.root_relation_id)
            .field(
                "known_object_relation_ids",
                &self
                    .known_object_sources
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .field(
                "identity_relation_ids",
                &self
                    .relation_identity_inputs
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .field(
                "directory_relation_ids",
                &self
                    .directory_relation_bounds
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .field(
                "related_relation_ids",
                &self
                    .related_relation_bindings
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .field(
                "unsupported_relation_ids",
                &self
                    .unsupported_observation_relations
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Negotiate, probe, discover, and compose one configured-root attachment in
/// the required RFC 012D order. `None` means the selected artifact is not
/// promoted for scoped observation; no legacy or durable fallback is opened.
pub(crate) fn prepare_configured_scoped_observation_attachment(
    registry: &AdapterRegistry,
    request: ScopedConfiguredAttachmentRequest,
) -> Result<Option<PreparedScopedObservationAttachment>, ScopedObservationAccessError> {
    request.validate()?;
    let Some(prepared) = prepare_scoped_observation_support(
        registry,
        &request.adapter_id,
        &request.configured_roots,
        &request.observation_contract_request,
        &request.observation_contract_offer,
        request.unknown_wire_contract.as_ref(),
    )?
    else {
        return Ok(None);
    };
    compose_prepared_attachment(prepared, request).map(Some)
}

fn compose_prepared_attachment(
    prepared: PreparedScopedObservationSupport,
    request: ScopedConfiguredAttachmentRequest,
) -> Result<PreparedScopedObservationAttachment, ScopedObservationAccessError> {
    let adapter = Arc::clone(&prepared.adapter);
    let adapter_id = prepared.adapter_id.clone();
    let external_reference_version = prepared
        .observation_contract
        .contract_versions
        .external_entity_reference_version;
    let program = prepared
        .authorization
        .select_scope_program(&request.program_id)
        .map_err(|_| invalid_configured_attachment())?;
    let plan = AuthorizedScopeAccessPlan::from_authorized_program(program)
        .map_err(|_| invalid_configured_attachment())?;
    let root_relation_id = plan.root_relation_id().to_string();

    let declared_known_objects = plan
        .known_object_relation_ids()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if request
        .known_object_relative_paths
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != declared_known_objects
    {
        return Err(invalid_configured_attachment());
    }

    let observation_relations = plan.observation_relations().cloned().collect::<Vec<_>>();
    let directory_relation_bounds = observation_relations
        .iter()
        .filter(|relation| relation.primitive == ScopeRelationPrimitive::ChildDirectoryByNativeId)
        .map(|relation| (relation.relation_id.clone(), relation.bounds))
        .collect::<BTreeMap<_, _>>();
    let related_relation_bindings = observation_relations
        .iter()
        .filter(|relation| {
            matches!(
                relation.primitive,
                ScopeRelationPrimitive::SiblingObject
                    | ScopeRelationPrimitive::ReferencedObjectFromField
            )
        })
        .map(|relation| {
            (
                relation.relation_id.clone(),
                PreparedScopedRelatedRelationBinding {
                    relation_id: relation.relation_id.clone(),
                    primitive: relation.primitive,
                    identity_input_names: relation.identity_inputs.clone(),
                    bounds: relation.bounds,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let unsupported_observation_relations = observation_relations
        .iter()
        .filter(|relation| {
            !matches!(
                relation.primitive,
                ScopeRelationPrimitive::KnownObject
                    | ScopeRelationPrimitive::ChildDirectoryByNativeId
                    | ScopeRelationPrimitive::SiblingObject
                    | ScopeRelationPrimitive::ReferencedObjectFromField
            )
        })
        .map(|relation| relation.relation_id.clone())
        .collect::<BTreeSet<_>>();
    let expected_identity_names = observation_relations
        .iter()
        .filter(|relation| {
            matches!(
                relation.primitive,
                ScopeRelationPrimitive::KnownObject
                    | ScopeRelationPrimitive::ChildDirectoryByNativeId
            )
        })
        .flat_map(|relation| relation.identity_inputs.iter().cloned())
        .collect::<BTreeSet<_>>();
    if request
        .identity
        .relation_identity_inputs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected_identity_names
    {
        return Err(invalid_configured_attachment());
    }
    let relation_identity_inputs = observation_relations
        .iter()
        .filter(|relation| {
            matches!(
                relation.primitive,
                ScopeRelationPrimitive::KnownObject
                    | ScopeRelationPrimitive::ChildDirectoryByNativeId
            )
        })
        .map(|relation| {
            Ok((
                relation.relation_id.clone(),
                request.identity.ordered_inputs(&relation.identity_inputs)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, ScopedObservationAccessError>>()?;

    let selected_artifacts = artifact_access::validate_artifact_relation_grants(
        &plan,
        request.artifact_relations.clone(),
    )?;
    let mut expected_access_roots = observation_relations
        .iter()
        .map(|relation| relation.access_root.clone())
        .collect::<BTreeSet<_>>();
    for relation_id in selected_artifacts.values() {
        let relation = plan
            .relation(relation_id)
            .ok_or_else(invalid_configured_attachment)?;
        expected_access_roots.insert(relation.access_root.clone());
    }

    let observed_at = now_unix_ms()?;
    let discovered = catch_unwind(AssertUnwindSafe(|| {
        adapter.discover(&DiscoveryContext {
            configured_roots: request.configured_roots.clone(),
            observed_at,
        })
    }))
    .map_err(|_| discovery_failed())?
    .map_err(|_| discovery_failed())?;
    validate_discovered_specs(&discovered)?;

    let mut selected = None;
    for (index, spec) in discovered.into_iter().enumerate() {
        let Some(instance) = scoped_instance_for_roots(spec, index, &expected_access_roots)? else {
            continue;
        };
        let root_identity = request.identity.root_request(&instance.spec);
        if root_identity
            .resolve(&adapter_id, external_reference_version)
            .is_err()
        {
            continue;
        }

        let mut known_objects = Vec::with_capacity(declared_known_objects.len());
        let mut known_object_sources = BTreeMap::new();
        for relation_id in &declared_known_objects {
            let relation = plan
                .relation(relation_id)
                .ok_or_else(invalid_configured_attachment)?;
            let relative_path = request
                .known_object_relative_paths
                .get(relation_id)
                .ok_or_else(invalid_configured_attachment)?;
            let stream = plan
                .validate_known_object_runtime_stream(
                    relation_id,
                    relative_path,
                    adapter.as_ref(),
                    &instance,
                )
                .map_err(|_| invalid_configured_attachment())?;
            let root = instance
                .root(&relation.access_root)
                .map_err(|_| invalid_configured_attachment())?
                .to_path_buf();
            known_objects.push(ScopedKnownObjectGrant {
                relation_id: relation_id.clone(),
                scope_root: relation_id == &root_relation_id,
                access_root: relation.access_root.clone(),
                locator_id: relation.locator.clone(),
                root,
                relative_path: relative_path.clone(),
            });
            known_object_sources.insert(
                relation_id.clone(),
                PreparedScopedKnownObjectSource {
                    relation_id: relation_id.clone(),
                    stream,
                    relative_path: relative_path.clone(),
                    max_bytes: relation.bounds.max_bytes,
                },
            );
        }
        let access_roots = expected_access_roots
            .iter()
            .map(|name| {
                Ok(ScopedAccessRootGrant {
                    access_root: name.clone(),
                    root: instance
                        .root(name)
                        .map_err(|_| invalid_configured_attachment())?
                        .to_path_buf(),
                })
            })
            .collect::<Result<Vec<_>, ScopedObservationAccessError>>()?;
        let candidate = (
            instance,
            root_identity,
            known_objects,
            access_roots,
            known_object_sources,
        );
        if selected.replace(candidate).is_some() {
            return Err(ambiguous_discovery());
        }
    }
    let (source_instance, root_identity, known_objects, access_roots, known_object_sources) =
        selected.ok_or(ScopedObservationAccessError::InvalidRootIdentity)?;
    let host = ScopedObservationAccessHost::authorize_prepared(
        prepared,
        ScopedObservationTrustedAccessRequest::new(
            source_instance,
            request.artifact_access_policy,
            root_identity,
            request.program_id,
            known_objects,
            access_roots,
            request.artifact_relations,
        ),
    )?;
    Ok(PreparedScopedObservationAttachment {
        host,
        root_relation_id,
        known_object_sources,
        directory_relation_bounds,
        related_relation_bindings,
        unsupported_observation_relations,
        relation_identity_inputs,
    })
}

fn validate_discovered_specs(
    specs: &[SourceInstanceSpec],
) -> Result<(), ScopedObservationAccessError> {
    if specs.is_empty() || specs.len() > MAX_DISCOVERED_SOURCE_INSTANCES {
        return Err(discovery_failed());
    }
    let mut stable_keys = BTreeSet::<SourceInstanceKey>::new();
    for spec in specs {
        let mut root_names = BTreeSet::new();
        if spec.validate().is_err()
            || !stable_keys.insert(spec.stable_key.clone())
            || spec.roots.is_empty()
            || spec.roots.iter().any(|root| {
                validate_relation_id(&root.name).is_err()
                    || root.path.as_os_str().is_empty()
                    || !root.path.is_absolute()
                    || !root_names.insert(root.name.as_str())
            })
        {
            return Err(discovery_failed());
        }
    }
    Ok(())
}

fn scoped_instance_for_roots(
    mut spec: SourceInstanceSpec,
    index: usize,
    expected_roots: &BTreeSet<String>,
) -> Result<Option<SourceInstance>, ScopedObservationAccessError> {
    if expected_roots
        .iter()
        .any(|name| spec.roots.iter().filter(|root| &root.name == name).count() != 1)
    {
        return Ok(None);
    }
    spec.roots
        .retain(|root| expected_roots.contains(&root.name));
    if spec.roots.len() != expected_roots.len() {
        return Ok(None);
    }
    let id = u64::try_from(index)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(discovery_failed)?;
    Ok(Some(SourceInstance { id, spec }))
}

fn now_unix_ms() -> Result<i64, ScopedObservationAccessError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .ok_or_else(discovery_failed)?;
    Ok(millis)
}

fn invalid_configured_attachment() -> ScopedObservationAccessError {
    ScopedObservationAccessError::InvalidGrant(
        "configured scoped attachment does not match its promoted authority".to_string(),
    )
}

fn discovery_failed() -> ScopedObservationAccessError {
    ScopedObservationAccessError::Authorization(
        "configured scoped attachment discovery failed".to_string(),
    )
}

fn ambiguous_discovery() -> ScopedObservationAccessError {
    ScopedObservationAccessError::InvalidGrant(
        "configured scoped attachment source selection is ambiguous".to_string(),
    )
}

fn invalid_configured_runtime() -> ScopedObservationAccessError {
    ScopedObservationAccessError::InvalidGrant(
        "configured scoped runtime does not match its prepared authority".to_string(),
    )
}

fn unsupported_configured_runtime() -> ScopedObservationAccessError {
    ScopedObservationAccessError::Authorization(
        "configured scoped runtime stream is not supported".to_string(),
    )
}

fn configured_decoder_binding_failed() -> ScopedObservationAccessError {
    ScopedObservationAccessError::Authorization(
        "configured scoped runtime decoder binding failed".to_string(),
    )
}
