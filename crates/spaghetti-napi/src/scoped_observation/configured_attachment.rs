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
    ExternalEntityRef, FactSemanticContext, NativeIdentityClaim, SourceInstance, SourceInstanceKey,
    SourceInstanceSpec, SourceObjectDescriptor, StreamSpec,
};
use crate::observation_contract::{ObservationContractOffer, ObservationContractRequest};
use crate::source::{
    confined_relative_path_key, platform_path_key, validate_relation_id, AppendDelimitedFile,
    AuthorizedScopeAccessPlan, RecordOrigin, SourceMediaType, MAX_IDENTITY_VALUE_BYTES,
};

use super::{
    artifact_access, prepare_scoped_observation_support, PreparedScopedObservationSupport,
    ScopedAccessRootGrant, ScopedAppendDecoderConfig, ScopedArtifactAccessPolicy,
    ScopedArtifactRelationGrant, ScopedKnownAppendObject, ScopedKnownObjectGrant,
    ScopedObservationAccessError, ScopedObservationAccessHost, ScopedObservationAppendPassBinding,
    ScopedObservationOwnedIdentityInput, ScopedObservationTrustedAccessRequest,
    ScopedObservationUnknownWireNegotiation, ScopedRootIdentityRequest,
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

    pub(crate) fn into_parts(
        self,
    ) -> (
        ScopedObservationAccessHost,
        Vec<ScopedKnownAppendObject>,
        Vec<ScopedObservationAppendPassBinding>,
    ) {
        (self.host, self.objects, self.bindings)
    }
}

impl std::fmt::Debug for PreparedConfiguredAppendRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedConfiguredAppendRuntime")
            .field("adapter_id", &self.host.root_identity().adapter_id)
            .field("session_key", &self.host.root_identity().session_key)
            .field("object_count", &self.objects.len())
            .field(
                "relation_ids",
                &self
                    .bindings
                    .iter()
                    .map(ScopedObservationAppendPassBinding::relation_id)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Fully composed store-free attachment authority. It has not opened a file,
/// installed a watcher, created a delivery drain, or started a pass.
pub(crate) struct PreparedScopedObservationAttachment {
    host: ScopedObservationAccessHost,
    root_relation_id: String,
    known_object_sources: BTreeMap<String, PreparedScopedKnownObjectSource>,
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
            relation_identity_inputs,
        } = self;
        if known_object_sources.len() != relation_identity_inputs.len() {
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
        Ok(PreparedConfiguredAppendRuntime {
            host,
            objects,
            bindings,
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
    let expected_identity_names = observation_relations
        .iter()
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
