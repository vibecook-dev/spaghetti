//! Database-free RFC 012 scoped-observation composition root.
//!
//! This module owns the seam between the strict adapter/support registry and
//! common source access. It deliberately exposes no N-API surface yet: native
//! artifact probing and the complete RFC 012D request contract must remain a
//! trusted Rust-host concern until their portable contracts are frozen.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::adapter::{
    ActorRunRole, AdapterError, AdapterErrorClass, AdapterId, AdapterObjectContext,
    AdapterRegistry, AgentAdapter, CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey,
    CompatibilityDecision, ContractCompleteness, ContractVersionOffer, ContractVersionRequest,
    CoverageAbsence, CoverageAbsenceKind, CoverageDeclarationDigest, CoverageDomain, CoverageError,
    CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoverageProvenance, CoverageScope,
    CoverageSetCompleteness, CoverageStatus, CoverageStreamKey, DecodeDisposition, DecoderId,
    ExternalEntityRef, Fact, FactBatch, FactEnvelope, FactProvenance, FactRevisionId,
    FactSemanticContext, FactSemanticRevision, NativeArtifactProbe, NativeIdentityClaim,
    QualifiedTimestamp, QualifiedValueQuality, RawRetentionPolicy, ScopeRelationPrimitive,
    SemanticRevisionRef, SourceAccess, SourceCoveragePoint, SourceCoverageSet, SourceObjectList,
    SourceObjectListRequest, SourceQuery, SourceRecordId, SourceRows, SourceSnapshot,
    SupportOperation, TypedAccessAuthorization, UsageRevisionV2Fact,
    EXTERNAL_ENTITY_REFERENCE_VERSION,
};
use crate::coverage_runtime::{
    derive_coverage_membership_revision, source_membership_prefix, CoverageMembershipObject,
};
use crate::decode_runtime::{
    decode_record, diagnostic_excerpt, DecodeRuntimeLimits, DecodeRuntimeRequest,
};
use crate::source::{
    confined_relative_path_key, read_stable_file_confined, AccessBudgetError, AccessObjectToken,
    AccessOperation, AccessOutcome, AccessPhase, AppendCheckpoint, AppendDelimitedFile, AppendItem,
    AppendRead, AppendTransition, AuthorizedScopeAccessPlan, DriverQuarantine, RecordHash,
    RecordOrigin, Revision, ScopeAccessReport, ScopeAccessRequest, ScopeIdentityInput,
    SourceCursor, SourceDriverError, SourceMediaType, SourceRecord, SourceRecordState, StableRead,
};

/// One exact host-approved object locator. The locator is installed during
/// attachment and cannot be replaced by a decoder or by an access call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedKnownObjectGrant {
    pub relation_id: String,
    pub access_root: String,
    pub locator_id: String,
    pub root: PathBuf,
    pub relative_path: PathBuf,
}

/// Internal attachment request. The artifact probe is supplied by the trusted
/// Rust composition root, never by a portable runtime consumer.
#[derive(Debug, Clone)]
pub struct ScopedObservationAccessRequest {
    pub adapter_id: String,
    pub artifact_probe: NativeArtifactProbe,
    pub contract_request: ContractVersionRequest,
    pub contract_offer: ContractVersionOffer,
    pub root_identity: ScopedRootIdentityRequest,
    pub program_id: String,
    pub known_objects: Vec<ScopedKnownObjectGrant>,
}

/// Pre-access root identity inputs selected by the trusted adapter/support
/// composition. Native/fallback bytes remain private and redacted from Debug;
/// only derived opaque common keys cross the observer boundary.
#[derive(Clone)]
pub struct ScopedRootIdentityRequest {
    source_instance_identity_contract_version: u32,
    stable_source_instance_discriminator: Arc<[u8]>,
    session_identity_key: Arc<[u8]>,
    root_run_identity_key: Option<Arc<[u8]>>,
    expected_session_key: Option<CanonicalEntityKey>,
    external_session_ref: Option<ExternalEntityRef>,
    native_session_claim: Option<NativeIdentityClaim>,
}

impl std::fmt::Debug for ScopedRootIdentityRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedRootIdentityRequest")
            .field(
                "source_instance_identity_contract_version",
                &self.source_instance_identity_contract_version,
            )
            .field(
                "has_declared_root_run_identity",
                &self.root_run_identity_key.is_some(),
            )
            .field(
                "has_expected_session_key",
                &self.expected_session_key.is_some(),
            )
            .field(
                "has_external_session_ref",
                &self.external_session_ref.is_some(),
            )
            .field(
                "has_native_session_claim",
                &self.native_session_claim.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ScopedRootIdentityRequest {
    pub fn new(
        source_instance_identity_contract_version: u32,
        stable_source_instance_discriminator: impl Into<Arc<[u8]>>,
        session_identity_key: impl Into<Arc<[u8]>>,
        root_run_identity_key: Option<Arc<[u8]>>,
        expected_session_key: Option<CanonicalEntityKey>,
        external_session_ref: Option<ExternalEntityRef>,
    ) -> Self {
        Self {
            source_instance_identity_contract_version,
            stable_source_instance_discriminator: stable_source_instance_discriminator.into(),
            session_identity_key: session_identity_key.into(),
            root_run_identity_key,
            expected_session_key,
            external_session_ref,
            native_session_claim: None,
        }
    }

    /// Attach an already-qualified native session claim. The claim is
    /// adjacent evidence only: its entity reference must equal the canonical
    /// root session derived from the pre-access identity inputs.
    pub fn with_native_session_claim(mut self, claim: NativeIdentityClaim) -> Self {
        self.native_session_claim = Some(claim);
        self
    }

    fn resolve(
        &self,
        adapter_id: &AdapterId,
        selected_external_reference_version: u32,
    ) -> Result<ScopedObservationRootIdentity, ScopedObservationAccessError> {
        if selected_external_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION {
            return Err(ScopedObservationAccessError::InvalidRootIdentity);
        }
        let source_instance_key = CanonicalSourceInstanceKey::derive(
            self.source_instance_identity_contract_version,
            &self.stable_source_instance_discriminator,
        )
        .map_err(|_| ScopedObservationAccessError::InvalidRootIdentity)?;
        let session_key = CanonicalEntityKey::derive(
            adapter_id.as_str(),
            &source_instance_key,
            "session",
            &self.session_identity_key,
        )
        .map_err(|_| ScopedObservationAccessError::InvalidRootIdentity)?;
        let root_actor_run_key = CanonicalEntityKey::derive_root_actor_run(
            adapter_id.as_str(),
            &source_instance_key,
            &session_key,
            self.root_run_identity_key.as_deref(),
        )
        .map_err(|_| ScopedObservationAccessError::InvalidRootIdentity)?;
        let session_ref = ExternalEntityRef::new(session_key);
        if self
            .expected_session_key
            .is_some_and(|expected| expected != session_key)
            || self
                .external_session_ref
                .is_some_and(|expected| expected != session_ref)
            || self
                .native_session_claim
                .as_ref()
                .is_some_and(|claim| claim.entity_ref != session_ref)
        {
            return Err(ScopedObservationAccessError::InvalidRootIdentity);
        }
        Ok(ScopedObservationRootIdentity {
            adapter_id: adapter_id.clone(),
            source_instance_key,
            session_key,
            session_ref,
            root_actor_run_key,
            native_session_claim: self.native_session_claim.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedObservationRootIdentity {
    pub adapter_id: AdapterId,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub session_key: CanonicalEntityKey,
    pub session_ref: ExternalEntityRef,
    pub root_actor_run_key: CanonicalEntityKey,
    pub native_session_claim: Option<NativeIdentityClaim>,
}

/// One read against an exact known-object grant. Native identity bytes are
/// consumed only to validate the declaration and derive the opaque audit token.
#[derive(Debug, Clone, Copy)]
pub struct ScopedKnownObjectReadRequest<'a> {
    pub relation_id: &'a str,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub depth: u32,
    pub max_bytes: u64,
}

/// One bounded append-driver invocation against an exact known-object grant.
#[derive(Debug, Clone, Copy)]
pub struct ScopedKnownAppendReadRequest<'a> {
    pub relation_id: &'a str,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub depth: u32,
    pub max_bytes: u64,
    pub previous: Option<&'a AppendCheckpoint>,
    pub origin: &'a RecordOrigin,
    pub force_contract_replay: bool,
}

/// Stateful root append reconciliation input. The checkpoint is intentionally
/// absent: the store-free kernel owns it and cannot accept a caller substitute.
#[derive(Debug, Clone, Copy)]
pub struct ScopedAppendReconcileRequest<'a> {
    pub relation_id: &'a str,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub access_phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub depth: u32,
    pub max_bytes: u64,
    pub origin: &'a RecordOrigin,
    pub force_contract_replay: bool,
}

/// Immutable decoder binding for one scoped append object. Selection is still
/// a trusted Rust-host responsibility until the complete RFC 012D root/stream
/// contract is frozen, but callers cannot switch decoder state domains between
/// reconciliations.
#[derive(Debug, Clone)]
pub struct ScopedAppendDecoderConfig {
    pub decoder: DecoderId,
    pub object_context: AdapterObjectContext,
    pub semantic_context: FactSemanticContext,
    /// Fact-family domains this exact object/decoder can cover. Decode
    /// coverage is implicit; projection-pack coverage is never legal here.
    pub coverage_domains: Vec<CoverageDomain>,
    pub retention: RawRetentionPolicy,
    pub max_facts_per_record: usize,
    pub max_diagnostics_per_record: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedAppendDeliveryPhase {
    Bootstrap,
    Live,
    Correction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedAppendReset {
    pub old_generation: u64,
    pub new_generation: u64,
    pub reason: AppendTransition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedAppendPresenceChange {
    Created { generation: u64 },
    Deleted { generation: u64 },
}

/// Stable, path-free source coordinate shared with RFC 012A coverage. Numeric
/// catalog IDs and attachment-local object tokens never cross this boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedSourceObjectIdentity {
    pub adapter_id: AdapterId,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub stream_key: CoverageStreamKey,
    pub object_key: CoverageObjectKey,
}

/// RFC 012A decode coverage for one scoped append object at the observer's
/// offered boundary. A present/unstable object contributes one point; a known
/// missing object contributes one explicit absence or deletion. The future
/// observer facade groups these bounded object entries into source coverage
/// sets after scope membership and supported fact-family domains are frozen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedOfferedDecodeCoverage {
    pub source: ScopedSourceObjectIdentity,
    pub point: Option<SourceCoveragePoint>,
    pub explicit_absence_or_deletion: Option<CoverageAbsence>,
    pub explicit_errors: Vec<CoverageError>,
    pub completeness: CoverageSetCompleteness,
}

impl ScopedSourceObjectIdentity {
    fn from_semantic_context(
        context: &FactSemanticContext,
    ) -> Result<Self, ScopedObservationAccessError> {
        let stream_namespace = std::str::from_utf8(context.stream_key())
            .map_err(|_| ScopedObservationAccessError::InvalidSemanticContext)?;
        let stream_key =
            CoverageStreamKey::derive(context.adapter_id().as_str(), context.stream_key())
                .map_err(|_| ScopedObservationAccessError::InvalidSemanticContext)?;
        let object_key = CoverageObjectKey::derive(stream_namespace, context.object_key())
            .map_err(|_| ScopedObservationAccessError::InvalidSemanticContext)?;
        Ok(Self {
            adapter_id: context.adapter_id().clone(),
            source_instance_key: context.source_instance_key(),
            stream_key,
            object_key,
        })
    }
}

fn source_belongs_to_root(
    source: &ScopedSourceObjectIdentity,
    root: &ScopedObservationRootIdentity,
) -> bool {
    source.adapter_id == root.adapter_id && source.source_instance_key == root.source_instance_key
}

#[derive(Debug, PartialEq, Eq)]
pub struct ScopedAppendObservation {
    object_token: u64,
    admission_token: u64,
    /// Host observation time supplied by the trusted source-driver request.
    /// It is delivery metadata and never participates in event identity.
    pub observed_at: i64,
    pub phase: ScopedAppendDeliveryPhase,
    pub reset_before_items: Option<ScopedAppendReset>,
    pub presence_change: Option<ScopedAppendPresenceChange>,
    pub root_present: bool,
    pub became_missing: bool,
    pub read: AppendRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedDecodedRecordEvidence {
    pub source_instance_id: u64,
    pub stream_id: u64,
    pub object_id: u64,
    pub generation: u64,
    pub cursor_start: SourceCursor,
    pub cursor_end: SourceCursor,
    pub ordinal_in_batch: u32,
    pub observed_at: i64,
    pub source_timestamp_hint: Option<i64>,
    pub media_type: SourceMediaType,
    pub state: SourceRecordState,
    pub payload_hash: RecordHash,
    /// Present only when the selected retention policy permits bounded native
    /// evidence. Hash-only/none never copy the raw source payload.
    pub retained_payload: Option<Vec<u8>>,
}

pub enum ScopedDecodedAppendItem {
    Record {
        evidence: Box<ScopedDecodedRecordEvidence>,
        disposition: DecodeDisposition,
        batch: FactBatch,
        quarantined: bool,
    },
    DriverQuarantine(DriverQuarantine),
}

pub struct ScopedDecodedAppendBatch {
    object_token: u64,
    admission_token: u64,
    pub items: Vec<ScopedDecodedAppendItem>,
}

pub enum ScopedAppendDecodeOutcome {
    Ready(ScopedDecodedAppendBatch),
    RetryTransient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationQueueLimits {
    pub max_data_events: u64,
    pub max_retained_native_bytes: u64,
    pub max_control_items: usize,
    /// Maximum stable source objects whose offered coverage may be retained.
    /// This is a scope-membership bound, independent of transient queue slots.
    pub max_coverage_objects: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedAdmissionError {
    #[error("scoped observation queue limits are invalid")]
    InvalidLimits,
    #[error("decoded batch does not belong to the pending observation")]
    ObservationMismatch,
    #[error("pending observation has not completed decoding")]
    ObservationNotDecoded,
    #[error("scoped decoded-data event capacity is full")]
    DataQueueFull,
    #[error("scoped retained-native byte capacity is full")]
    RetainedNativeQueueFull,
    #[error("scoped lifecycle/control capacity is full")]
    ControlQueueFull,
    #[error("scoped offered-coverage object capacity is full")]
    CoverageObjectCapacityFull,
    #[error("scoped append coverage could not be represented by the common contract")]
    InvalidCoverage,
    #[error("scoped admission accounting or sequence range is exhausted")]
    CapacityExhausted,
}

pub struct ScopedAdmissionFailure {
    pub error: ScopedAdmissionError,
    pub decoded: ScopedDecodedAppendBatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedAdmissionReceipt {
    object_token: u64,
    admission_token: u64,
    pub through_lane_ordinal: u64,
    pub data_events: u64,
    pub retained_native_bytes: u64,
    pub control_items: u32,
}

pub enum ScopedQueuedObservationFrame {
    Presence {
        object_token: u64,
        source: ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        observed_at: i64,
        phase: ScopedAppendDeliveryPhase,
        change: ScopedAppendPresenceChange,
    },
    Reset {
        object_token: u64,
        source: ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        observed_at: i64,
        phase: ScopedAppendDeliveryPhase,
        reset: ScopedAppendReset,
    },
    Decoded {
        object_token: u64,
        source: ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        phase: ScopedAppendDeliveryPhase,
        item: Box<ScopedDecodedAppendItem>,
    },
}

impl ScopedQueuedObservationFrame {
    pub fn lane_ordinal(&self) -> u64 {
        match self {
            Self::Presence { lane_ordinal, .. }
            | Self::Reset { lane_ordinal, .. }
            | Self::Decoded { lane_ordinal, .. } => *lane_ordinal,
        }
    }
}

struct QueuedDecodedFrame {
    object_token: u64,
    source: ScopedSourceObjectIdentity,
    lane_ordinal: u64,
    phase: ScopedAppendDeliveryPhase,
    item: Box<ScopedDecodedAppendItem>,
    data_events: u64,
    retained_native_bytes: u64,
}

#[derive(Clone, Copy)]
enum QueuedControlKind {
    Presence(ScopedAppendPresenceChange),
    Reset(ScopedAppendReset),
}

#[derive(Clone)]
struct QueuedControlFrame {
    object_token: u64,
    source: ScopedSourceObjectIdentity,
    lane_ordinal: u64,
    observed_at: i64,
    phase: ScopedAppendDeliveryPhase,
    kind: QueuedControlKind,
}

/// Temporary ownership of exactly one admitted frame. Removing a frame from
/// its deque does not release admission accounting; the frame can therefore
/// be restored byte-for-byte when projection or delivery preflight fails.
struct ScopedTakenObservationFrame {
    frame: ScopedQueuedObservationFrame,
    data_events: u64,
    retained_native_bytes: u64,
}

struct PendingScopedCoverageUpdate {
    through_lane_ordinal: u64,
    coverage: ScopedOfferedDecodeCoverage,
}

#[derive(Clone, PartialEq, Eq)]
struct ScopedCoverageMembershipIdentity {
    stream_key: Arc<[u8]>,
    object_key: Arc<[u8]>,
    coverage_domains: Vec<CoverageDomain>,
}

/// Two bounded internal capacity domains multiplexed by one admission ordinal.
/// The ordinal is not an RFC 012D `observer_sequence`; public sequencing begins
/// only after canonical semantic projection is available.
pub struct ScopedObservationAdmissionLane {
    limits: ScopedObservationQueueLimits,
    decoded: VecDeque<QueuedDecodedFrame>,
    controls: VecDeque<QueuedControlFrame>,
    queued_data_events: u64,
    queued_retained_native_bytes: u64,
    next_lane_ordinal: u64,
    offered_lane_ordinal: u64,
    known_coverage_objects: BTreeMap<ScopedSourceObjectIdentity, ScopedCoverageMembershipIdentity>,
    pending_coverage_updates: VecDeque<PendingScopedCoverageUpdate>,
    offered_decode_coverage: BTreeMap<ScopedSourceObjectIdentity, ScopedOfferedDecodeCoverage>,
}

impl ScopedObservationAdmissionLane {
    pub fn new(limits: ScopedObservationQueueLimits) -> Result<Self, ScopedAdmissionError> {
        if limits.max_data_events == 0
            || limits.max_control_items == 0
            || limits.max_coverage_objects == 0
        {
            return Err(ScopedAdmissionError::InvalidLimits);
        }
        Ok(Self {
            limits,
            decoded: VecDeque::new(),
            controls: VecDeque::new(),
            queued_data_events: 0,
            queued_retained_native_bytes: 0,
            next_lane_ordinal: 1,
            offered_lane_ordinal: 0,
            known_coverage_objects: BTreeMap::new(),
            pending_coverage_updates: VecDeque::new(),
            offered_decode_coverage: BTreeMap::new(),
        })
    }

    /// Atomically admit the reset control and decoded data, then commit the
    /// matching object's cursor/decoder state. Capacity failure returns the
    /// still-owned decoded batch and leaves the object unchanged.
    pub fn admit(
        &mut self,
        object: &mut ScopedKnownAppendObject,
        observation: &ScopedAppendObservation,
        mut decoded: ScopedDecodedAppendBatch,
    ) -> Result<ScopedAdmissionReceipt, ScopedAdmissionFailure> {
        if let Err(error) = object.validate_admission(observation, &decoded) {
            return Err(ScopedAdmissionFailure {
                error: admission_validation_error(error),
                decoded,
            });
        }
        let coverage = match object.prepare_decode_coverage(observation, &decoded) {
            Ok(coverage) => coverage,
            Err(()) => {
                return Err(ScopedAdmissionFailure {
                    error: ScopedAdmissionError::InvalidCoverage,
                    decoded,
                });
            }
        };
        let membership_identity = object.coverage_membership_identity();
        let source_is_new = !self.known_coverage_objects.contains_key(&object.source);
        if self
            .known_coverage_objects
            .get(&object.source)
            .is_some_and(|known| known != &membership_identity)
        {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::InvalidCoverage,
                decoded,
            });
        }
        if source_is_new && self.known_coverage_objects.len() >= self.limits.max_coverage_objects {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::CoverageObjectCapacityFull,
                decoded,
            });
        }

        let mut measurements = Vec::with_capacity(decoded.items.len());
        let mut data_events = 0_u64;
        let mut retained_native_bytes = 0_u64;
        for item in &decoded.items {
            let measurement = match decoded_item_measurement(item) {
                Some(measurement) => measurement,
                None => {
                    return Err(ScopedAdmissionFailure {
                        error: ScopedAdmissionError::CapacityExhausted,
                        decoded,
                    });
                }
            };
            let Some(next_events) = data_events.checked_add(measurement.0) else {
                return Err(ScopedAdmissionFailure {
                    error: ScopedAdmissionError::CapacityExhausted,
                    decoded,
                });
            };
            let Some(next_bytes) = retained_native_bytes.checked_add(measurement.1) else {
                return Err(ScopedAdmissionFailure {
                    error: ScopedAdmissionError::CapacityExhausted,
                    decoded,
                });
            };
            data_events = next_events;
            retained_native_bytes = next_bytes;
            measurements.push(measurement);
        }
        let control_items = usize::from(observation.presence_change.is_some())
            + usize::from(observation.reset_before_items.is_some());
        if self
            .queued_data_events
            .checked_add(data_events)
            .is_none_or(|value| value > self.limits.max_data_events)
        {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::DataQueueFull,
                decoded,
            });
        }
        if self
            .queued_retained_native_bytes
            .checked_add(retained_native_bytes)
            .is_none_or(|value| value > self.limits.max_retained_native_bytes)
        {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::RetainedNativeQueueFull,
                decoded,
            });
        }
        let Some(next_control_items) = self.controls.len().checked_add(control_items) else {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::CapacityExhausted,
                decoded,
            });
        };
        if next_control_items > self.limits.max_control_items {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::ControlQueueFull,
                decoded,
            });
        }
        let Some(frame_count) = control_items.checked_add(decoded.items.len()) else {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::CapacityExhausted,
                decoded,
            });
        };
        let Ok(frame_count) = u64::try_from(frame_count) else {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::CapacityExhausted,
                decoded,
            });
        };
        let Some(after_ordinal) = self.next_lane_ordinal.checked_add(frame_count) else {
            return Err(ScopedAdmissionFailure {
                error: ScopedAdmissionError::CapacityExhausted,
                decoded,
            });
        };

        let mut lane_ordinal = self.next_lane_ordinal;
        if let Some(change) = observation.presence_change {
            self.controls.push_back(QueuedControlFrame {
                object_token: observation.object_token,
                source: object.source.clone(),
                lane_ordinal,
                observed_at: observation.observed_at,
                phase: observation.phase,
                kind: QueuedControlKind::Presence(change),
            });
            lane_ordinal += 1;
        }
        if let Some(reset) = observation.reset_before_items {
            self.controls.push_back(QueuedControlFrame {
                object_token: observation.object_token,
                source: object.source.clone(),
                lane_ordinal,
                observed_at: observation.observed_at,
                phase: observation.phase,
                kind: QueuedControlKind::Reset(reset),
            });
            lane_ordinal += 1;
        }
        for (item, (item_events, item_bytes)) in decoded.items.drain(..).zip(measurements) {
            self.decoded.push_back(QueuedDecodedFrame {
                object_token: observation.object_token,
                source: object.source.clone(),
                lane_ordinal,
                phase: observation.phase,
                item: Box::new(item),
                data_events: item_events,
                retained_native_bytes: item_bytes,
            });
            lane_ordinal += 1;
        }
        debug_assert_eq!(lane_ordinal, after_ordinal);
        self.next_lane_ordinal = after_ordinal;
        self.queued_data_events += data_events;
        self.queued_retained_native_bytes += retained_native_bytes;
        object.commit_admission();

        if source_is_new {
            self.known_coverage_objects
                .insert(object.source.clone(), membership_identity);
        }
        self.stage_coverage_update(
            after_ordinal
                .checked_sub(1)
                .expect("scoped lane ordinals start at one"),
            coverage,
        );

        Ok(ScopedAdmissionReceipt {
            object_token: observation.object_token,
            admission_token: observation.admission_token,
            through_lane_ordinal: after_ordinal
                .checked_sub(1)
                .expect("scoped lane ordinals start at one"),
            data_events,
            retained_native_bytes,
            control_items: control_items as u32,
        })
    }

    /// Test-only low-level ownership transfer for admission/order conformance.
    /// Production composition must use `offer_next`, which keeps admission,
    /// projection, and bounded delivery in one retry-safe transaction.
    #[cfg(test)]
    pub fn pop_next(&mut self) -> Option<ScopedQueuedObservationFrame> {
        let taken = self.take_next_frame()?;
        Some(self.commit_taken_frame(taken))
    }

    /// Test-only projection seam. Production must not release admission
    /// accounting before the exact projected batch has entered delivery.
    #[cfg(test)]
    pub fn project_next(
        &mut self,
        projection: &mut ScopedObservationProjectionSink,
    ) -> Result<Option<Vec<ScopedProjectedObservation>>, ScopedProjectionError> {
        let Some(taken) = self.take_next_frame() else {
            return Ok(None);
        };
        let plan = match projection.prepare(&taken.frame) {
            Ok(plan) => plan,
            Err(error) => {
                self.restore_taken_frame(taken);
                return Err(error);
            }
        };
        let ScopedProjectionPlan {
            projected,
            mutation,
        } = plan;
        projection.commit(mutation);
        self.commit_taken_frame(taken);
        Ok(Some(projected))
    }

    /// Atomically advance one admitted frame through semantic projection and
    /// bounded delivery. Projection first prepares an exact batch without
    /// mutating reducer state. Delivery then performs its all-or-nothing
    /// capacity check and offer. Only after that succeeds do the reducer
    /// mutation and admission-accounting release commit synchronously.
    ///
    /// Either error leaves the frame at the head of its original queue, keeps
    /// reducer state unchanged, and advances no offered observer sequence. An
    /// exact semantic repeat prepares an empty batch, so it can retire even
    /// while the semantic delivery queue is full.
    pub fn offer_next(
        &mut self,
        projection: &mut ScopedObservationProjectionSink,
        delivery: &mut ScopedObservationDeliveryLane,
    ) -> Result<Option<ScopedObservationOfferReceipt>, ScopedProjectionDeliveryError> {
        if projection.lifecycle != ScopedProjectionLifecycle::Active
            || delivery.state().continuity == ScopedObservationContinuity::Resyncing
        {
            return Err(ScopedProjectionDeliveryError::Projection(
                ScopedProjectionError::InvalidLifecycle,
            ));
        }
        let Some(taken) = self.take_next_frame() else {
            return Ok(None);
        };
        let plan = match projection.prepare(&taken.frame) {
            Ok(plan) => plan,
            Err(error) => {
                self.restore_taken_frame(taken);
                return Err(ScopedProjectionDeliveryError::Projection(error));
            }
        };
        let ScopedProjectionPlan {
            projected,
            mutation,
        } = plan;
        let receipt = match delivery.offer_projected(projected) {
            Ok(receipt) => receipt,
            Err(failure) => {
                self.restore_taken_frame(taken);
                return Err(ScopedProjectionDeliveryError::Delivery(failure.error));
            }
        };
        projection.commit(mutation);
        self.commit_taken_frame(taken);
        Ok(Some(receipt))
    }

    fn take_next_frame(&mut self) -> Option<ScopedTakenObservationFrame> {
        let take_control = self.next_is_control()?;
        if take_control {
            let control = self.controls.pop_front().expect("control front exists");
            return Some(ScopedTakenObservationFrame {
                frame: queued_control_observation(control),
                data_events: 0,
                retained_native_bytes: 0,
            });
        }
        let decoded = self.decoded.pop_front().expect("decoded front exists");
        Some(ScopedTakenObservationFrame {
            frame: ScopedQueuedObservationFrame::Decoded {
                object_token: decoded.object_token,
                source: decoded.source,
                lane_ordinal: decoded.lane_ordinal,
                phase: decoded.phase,
                item: decoded.item,
            },
            data_events: decoded.data_events,
            retained_native_bytes: decoded.retained_native_bytes,
        })
    }

    fn restore_taken_frame(&mut self, taken: ScopedTakenObservationFrame) {
        match taken.frame {
            ScopedQueuedObservationFrame::Presence {
                object_token,
                source,
                lane_ordinal,
                observed_at,
                phase,
                change,
            } => self.controls.push_front(QueuedControlFrame {
                object_token,
                source,
                lane_ordinal,
                observed_at,
                phase,
                kind: QueuedControlKind::Presence(change),
            }),
            ScopedQueuedObservationFrame::Reset {
                object_token,
                source,
                lane_ordinal,
                observed_at,
                phase,
                reset,
            } => self.controls.push_front(QueuedControlFrame {
                object_token,
                source,
                lane_ordinal,
                observed_at,
                phase,
                kind: QueuedControlKind::Reset(reset),
            }),
            ScopedQueuedObservationFrame::Decoded {
                object_token,
                source,
                lane_ordinal,
                phase,
                item,
            } => self.decoded.push_front(QueuedDecodedFrame {
                object_token,
                source,
                lane_ordinal,
                phase,
                item,
                data_events: taken.data_events,
                retained_native_bytes: taken.retained_native_bytes,
            }),
        }
    }

    fn commit_taken_frame(
        &mut self,
        taken: ScopedTakenObservationFrame,
    ) -> ScopedQueuedObservationFrame {
        let lane_ordinal = taken.frame.lane_ordinal();
        if matches!(&taken.frame, ScopedQueuedObservationFrame::Decoded { .. }) {
            self.queued_data_events = self
                .queued_data_events
                .checked_sub(taken.data_events)
                .expect("queued decoded event accounting cannot underflow");
            self.queued_retained_native_bytes = self
                .queued_retained_native_bytes
                .checked_sub(taken.retained_native_bytes)
                .expect("queued retained-native accounting cannot underflow");
        }
        self.offered_lane_ordinal = lane_ordinal;
        self.apply_coverage_updates_through(lane_ordinal);
        taken.frame
    }

    fn stage_coverage_update(
        &mut self,
        through_lane_ordinal: u64,
        coverage: ScopedOfferedDecodeCoverage,
    ) {
        if through_lane_ordinal <= self.offered_lane_ordinal {
            self.offered_decode_coverage
                .insert(coverage.source.clone(), coverage);
            return;
        }

        // Several event-free reads can complete behind the same queued frame.
        // Only the latest source state at that boundary matters, so coalescing
        // prevents no-op polling from creating an unbounded marker queue.
        if let Some(pending) = self
            .pending_coverage_updates
            .iter_mut()
            .rev()
            .find(|pending| {
                pending.through_lane_ordinal == through_lane_ordinal
                    && pending.coverage.source == coverage.source
            })
        {
            pending.coverage = coverage;
            return;
        }
        self.pending_coverage_updates
            .push_back(PendingScopedCoverageUpdate {
                through_lane_ordinal,
                coverage,
            });
    }

    fn apply_coverage_updates_through(&mut self, through_lane_ordinal: u64) {
        while self
            .pending_coverage_updates
            .front()
            .is_some_and(|pending| pending.through_lane_ordinal <= through_lane_ordinal)
        {
            let pending = self
                .pending_coverage_updates
                .pop_front()
                .expect("front coverage update was just observed");
            self.offered_decode_coverage
                .insert(pending.coverage.source.clone(), pending.coverage);
        }
    }

    pub fn queued_data_events(&self) -> u64 {
        self.queued_data_events
    }

    pub fn queued_retained_native_bytes(&self) -> u64 {
        self.queued_retained_native_bytes
    }

    pub fn queued_control_items(&self) -> usize {
        self.controls.len()
    }

    pub fn queued_coverage_updates(&self) -> usize {
        self.pending_coverage_updates.len()
    }

    /// Latest decode coverage whose corresponding control/data frames have all
    /// crossed the offered boundary. A newer admitted source checkpoint may
    /// exist while this value intentionally remains unchanged.
    pub fn offered_decode_coverage(
        &self,
        source: &ScopedSourceObjectIdentity,
    ) -> Option<&ScopedOfferedDecodeCoverage> {
        self.offered_decode_coverage.get(source)
    }

    pub fn is_empty(&self) -> bool {
        self.controls.is_empty()
            && self.decoded.is_empty()
            && self.pending_coverage_updates.is_empty()
    }

    fn next_is_control(&self) -> Option<bool> {
        match (self.controls.front(), self.decoded.front()) {
            (Some(control), Some(decoded)) => Some(control.lane_ordinal < decoded.lane_ordinal),
            (Some(_), None) => Some(true),
            (None, Some(_)) => Some(false),
            (None, None) => None,
        }
    }
}

fn queued_control_observation(control: QueuedControlFrame) -> ScopedQueuedObservationFrame {
    match control.kind {
        QueuedControlKind::Presence(change) => ScopedQueuedObservationFrame::Presence {
            object_token: control.object_token,
            source: control.source,
            lane_ordinal: control.lane_ordinal,
            observed_at: control.observed_at,
            phase: control.phase,
            change,
        },
        QueuedControlKind::Reset(reset) => ScopedQueuedObservationFrame::Reset {
            object_token: control.object_token,
            source: control.source,
            lane_ordinal: control.lane_ordinal,
            observed_at: control.observed_at,
            phase: control.phase,
            reset,
        },
    }
}

/// Provisional internal event-contract version. These values are not exposed
/// over N-API until the complete RFC 012D envelope and negotiation surface are
/// frozen, but event identity is already derived with the normative inputs.
pub const SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION: u32 = 1;
pub const SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION: u32 = 1;
pub const SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION: u32 = 1;
pub const SCOPED_RESYNC_BARRIER_CONTRACT_VERSION: u32 = 1;
pub const RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION: u32 = 1;
pub const SCOPED_INITIAL_SCOPE_EPOCH: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopedObservationEventId([u8; 32]);

impl ScopedObservationEventId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopedReplacementSemanticDigest([u8; 32]);

impl ScopedReplacementSemanticDigest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopedBootstrapSnapshotDigest([u8; 32]);

impl ScopedBootstrapSnapshotDigest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopedReplacementSnapshotDigest([u8; 32]);

impl ScopedReplacementSnapshotDigest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedUsageV2Operation {
    Upsert,
    Retract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedUsageV2RetractionCause {
    Reset(ScopedAppendReset),
    SourceDeleted { generation: u64 },
}

/// Canonical occurrence/provenance retained by the store-free semantic
/// reducer. Native payload bytes remain owned by the bounded admission frame;
/// reducer state never duplicates them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedUsageV2Source {
    pub object: ScopedSourceObjectIdentity,
    pub source_record_id: SourceRecordId,
    pub provenance: FactProvenance,
    pub cursor_start: SourceCursor,
    pub cursor_end: SourceCursor,
    pub ordinal_in_batch: u32,
    pub source_timestamp_hint: Option<i64>,
    pub media_type: SourceMediaType,
    pub state: SourceRecordState,
    pub payload_hash: RecordHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedUsageV2Event {
    pub event_id: ScopedObservationEventId,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub fact_id: CanonicalFactId,
    pub operation: ScopedUsageV2Operation,
    pub phase: ScopedAppendDeliveryPhase,
    /// Host time for this accepted delivery occurrence. For lifecycle-owned
    /// retractions this is the control observation time, not the retracted
    /// record's earlier provenance time.
    pub observed_at: i64,
    pub source: ScopedUsageV2Source,
    /// Present only for a reducer retraction caused by source lifecycle.
    pub retraction: Option<ScopedUsageV2RetractionCause>,
    /// The accepted response revision. Retractions carry the revision being
    /// removed so actor/session routing never has to guess from a control.
    pub revision: UsageRevisionV2Fact,
}

/// Family-level replacement primitive used by clean bootstrap and future
/// resync staging. Coverage/completeness remain barrier-owned; this value
/// proves only the complete current reducer state known to this sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedUsageV2ReplacementSnapshot {
    pub fact_family_contract_version: u32,
    pub replacement_digest_contract_version: u32,
    pub phase: ScopedAppendDeliveryPhase,
    pub entity_count: u64,
    pub semantic_digest: ScopedReplacementSemanticDigest,
    pub events: Vec<ScopedUsageV2Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScopedReplacementRepresentation {
    UsageLatestContributionPerResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedReplacementFamilyManifest {
    pub fact_family: String,
    pub contract_version: u32,
    pub replacement_representation: ScopedReplacementRepresentation,
    pub completeness: CoverageSetCompleteness,
    pub entity_or_event_count: u64,
    pub semantic_digest: ScopedReplacementSemanticDigest,
}

/// Immutable engine-level bootstrap admission barrier. Queue state is captured
/// immediately after the completion control itself enters the ordered lane;
/// it proves offer through `barrier_sequence`, never consumer application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedBootstrapBarrier {
    pub barrier_contract_version: u32,
    pub root: ScopedObservationRootIdentity,
    pub scope_epoch: u64,
    pub barrier_sequence: u64,
    pub snapshot_digest: ScopedBootstrapSnapshotDigest,
    pub source_coverage: Vec<SourceCoverageSet>,
    pub explicit_object_errors: Vec<CoverageError>,
    pub queue_state: ScopedObservationDeliveryState,
    pub root_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedResyncReason {
    WatcherOverflow,
    TransportContinuityLoss,
    ExplicitConsumerRequest,
}

/// Sticky invalidation of one scope epoch. Discard accounting is diagnostic
/// only and is excluded from deterministic control identity because it depends
/// on consumer delivery speed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedResyncRequired {
    pub root: ScopedObservationRootIdentity,
    pub invalid_scope_epoch: u64,
    pub control_sequence: u64,
    pub last_contiguous_sequence: u64,
    pub baseline_snapshot_digest: ScopedBootstrapSnapshotDigest,
    pub reason: ScopedResyncReason,
    pub discarded_semantic_events: u64,
    pub discarded_source_controls: u64,
    pub discarded_retained_native_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedReplacementMode {
    FullSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedResyncStarted {
    pub root: ScopedObservationRootIdentity,
    pub old_scope_epoch: u64,
    pub new_scope_epoch: u64,
    pub control_sequence: u64,
    pub required_control_sequence: u64,
    pub baseline_snapshot_digest: ScopedBootstrapSnapshotDigest,
    pub reason: ScopedResyncReason,
    pub replacement: ScopedReplacementMode,
}

/// Offered-boundary completion of one isolated replacement. The family
/// manifest makes absence actionable for the families this provisional sink
/// supports; source coverage preserves partial/unavailable evidence instead
/// of silently carrying old-epoch entities forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedResyncBarrier {
    pub barrier_contract_version: u32,
    pub root: ScopedObservationRootIdentity,
    pub scope_epoch: u64,
    pub replacement: ScopedReplacementMode,
    pub started_control_sequence: u64,
    pub barrier_sequence: u64,
    pub replacement_snapshot_digest: ScopedReplacementSnapshotDigest,
    pub coverage_snapshot_digest: ScopedBootstrapSnapshotDigest,
    pub family_manifest: Vec<ScopedReplacementFamilyManifest>,
    pub source_coverage: Vec<SourceCoverageSet>,
    pub explicit_object_errors: Vec<CoverageError>,
    pub queue_state: ScopedObservationDeliveryState,
    pub root_present: bool,
}

/// First common semantic output of the scoped observation path. The reset
/// frame remains a control input for the future ordered public multiplexer;
/// usage events already have final deterministic event IDs and canonical
/// semantic references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedProjectedObservation {
    SourcePresence {
        object_token: u64,
        source: ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        observed_at: i64,
        phase: ScopedAppendDeliveryPhase,
        event_id: ScopedObservationEventId,
        change: ScopedAppendPresenceChange,
    },
    SourceReset {
        object_token: u64,
        source: ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        observed_at: i64,
        phase: ScopedAppendDeliveryPhase,
        event_id: ScopedObservationEventId,
        reset: ScopedAppendReset,
    },
    UsageV2 {
        lane_ordinal: u64,
        event: Box<ScopedUsageV2Event>,
    },
    ObserverBootstrapComplete {
        source: ScopedSourceObjectIdentity,
        observed_at: i64,
        event_id: ScopedObservationEventId,
        barrier: Arc<ScopedBootstrapBarrier>,
    },
    ObserverResyncRequired {
        source: ScopedSourceObjectIdentity,
        observed_at: i64,
        event_id: ScopedObservationEventId,
        control: Arc<ScopedResyncRequired>,
    },
    ObserverResyncStarted {
        source: ScopedSourceObjectIdentity,
        observed_at: i64,
        event_id: ScopedObservationEventId,
        control: Arc<ScopedResyncStarted>,
    },
    ObserverResyncComplete {
        source: ScopedSourceObjectIdentity,
        observed_at: i64,
        event_id: ScopedObservationEventId,
        barrier: Arc<ScopedResyncBarrier>,
    },
}

impl ScopedProjectedObservation {
    pub fn event_id(&self) -> ScopedObservationEventId {
        match self {
            Self::SourcePresence { event_id, .. } | Self::SourceReset { event_id, .. } => *event_id,
            Self::UsageV2 { event, .. } => event.event_id,
            Self::ObserverBootstrapComplete { event_id, .. } => *event_id,
            Self::ObserverResyncRequired { event_id, .. } => *event_id,
            Self::ObserverResyncStarted { event_id, .. } => *event_id,
            Self::ObserverResyncComplete { event_id, .. } => *event_id,
        }
    }

    pub fn semantic_revision_ref(&self) -> Option<SemanticRevisionRef> {
        match self {
            Self::UsageV2 { event, .. } => Some(event.semantic_revision_ref),
            Self::SourcePresence { .. }
            | Self::SourceReset { .. }
            | Self::ObserverBootstrapComplete { .. }
            | Self::ObserverResyncRequired { .. }
            | Self::ObserverResyncStarted { .. }
            | Self::ObserverResyncComplete { .. } => None,
        }
    }

    pub fn phase(&self) -> ScopedAppendDeliveryPhase {
        match self {
            Self::SourcePresence { phase, .. } | Self::SourceReset { phase, .. } => *phase,
            Self::UsageV2 { event, .. } => event.phase,
            Self::ObserverBootstrapComplete { .. } => ScopedAppendDeliveryPhase::Bootstrap,
            Self::ObserverResyncRequired { .. } => ScopedAppendDeliveryPhase::Live,
            Self::ObserverResyncStarted { .. } => ScopedAppendDeliveryPhase::Correction,
            Self::ObserverResyncComplete { .. } => ScopedAppendDeliveryPhase::Correction,
        }
    }

    pub fn source(&self) -> &ScopedSourceObjectIdentity {
        match self {
            Self::SourcePresence { source, .. } | Self::SourceReset { source, .. } => source,
            Self::UsageV2 { event, .. } => &event.source.object,
            Self::ObserverBootstrapComplete { source, .. } => source,
            Self::ObserverResyncRequired { source, .. } => source,
            Self::ObserverResyncStarted { source, .. } => source,
            Self::ObserverResyncComplete { source, .. } => source,
        }
    }

    pub fn observed_at(&self) -> i64 {
        match self {
            Self::SourcePresence { observed_at, .. } | Self::SourceReset { observed_at, .. } => {
                *observed_at
            }
            Self::UsageV2 { event, .. } => event.observed_at,
            Self::ObserverBootstrapComplete { observed_at, .. } => *observed_at,
            Self::ObserverResyncRequired { observed_at, .. } => *observed_at,
            Self::ObserverResyncStarted { observed_at, .. } => *observed_at,
            Self::ObserverResyncComplete { observed_at, .. } => *observed_at,
        }
    }
}

/// Bounded post-reducer capacity. Native bytes retained during decode remain
/// charged to the admission lane until projection succeeds; this second byte
/// budget is for future projected unknown/native evidence that is intentionally
/// carried into delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationDeliveryLimits {
    pub max_semantic_events: usize,
    pub max_retained_native_bytes: u64,
    pub max_source_control_items: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationOfferReceipt {
    pub first_offered_sequence: Option<u64>,
    pub offered_through_sequence: u64,
    pub semantic_events: u64,
    pub retained_native_bytes: u64,
    pub source_control_items: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedDeliveredObservation {
    pub event_contract_version: u32,
    /// Delivery order within this attachment. It is assigned at the offered
    /// boundary and is neither a semantic identity nor stable across attaches.
    pub observer_sequence: u64,
    pub scope_epoch: u64,
    pub event_id: ScopedObservationEventId,
    pub semantic_revision_ref: Option<SemanticRevisionRef>,
    pub phase: ScopedAppendDeliveryPhase,
    pub source: ScopedSourceObjectIdentity,
    pub event: ScopedProjectedObservation,
}

/// Sanitized RFC 012D root routing carried by every delivered envelope.
/// Native identity remains an optional qualified claim and never replaces the
/// canonical session key/reference pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedObservationEnvelopeRoot {
    pub session_ref: ExternalEntityRef,
    pub session_key: CanonicalEntityKey,
    pub native_session_claim: Option<NativeIdentityClaim>,
}

/// Topology-neutral RFC 012C actor reference available at delivery time.
/// Parent/native actor attributes remain absent until their semantic revisions
/// have reached the scoped actor reducer; absence is not guessed from paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedActorRunRef {
    pub root_session_key: CanonicalEntityKey,
    pub run_key: CanonicalEntityKey,
    pub role: ActorRunRole,
    pub parent_run_key: Option<CanonicalEntityKey>,
    pub native_session_id: Option<String>,
    pub native_actor_id: Option<String>,
    pub native_actor_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedActorFallbackReason {
    SourceLifecycleControl,
    ObserverLifecycleControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedActorAttribution {
    NativeExact,
    DerivedExact,
    ScopeFallback { reason: ScopedActorFallbackReason },
}

/// Current affiliation context adjacent to an event. Until affiliation-family
/// projection lands, the mapper reports Unknown rather than treating an empty
/// vector as proof that no team/workflow relation exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedActorAffiliationContext {
    pub actor_run_key: CanonicalEntityKey,
    pub team_key: Option<CanonicalEntityKey>,
    pub native_team_id: Option<String>,
    pub team_name: Option<String>,
    pub member_key: Option<CanonicalEntityKey>,
    pub workflow_key: Option<CanonicalEntityKey>,
    pub native_workflow_id: Option<String>,
    pub completeness: ContractCompleteness,
    pub derived_from_revision_refs: Vec<SemanticRevisionRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationByteRange {
    pub start: u64,
    pub end: u64,
}

/// Path-free source occurrence. `locator_id` is reserved for a future
/// policy-approved opaque artifact locator; native paths never enter this DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedObservationEnvelopeSource {
    pub instance_key: CanonicalSourceInstanceKey,
    pub stream_key: CoverageStreamKey,
    pub object_key: CoverageObjectKey,
    pub locator_id: Option<String>,
    pub generation: u64,
    pub source_record_id: Option<SourceRecordId>,
    pub record_index: Option<u32>,
    pub cursor_start: Option<SourceCursor>,
    pub cursor_end: Option<SourceCursor>,
    pub byte_range: Option<ScopedObservationByteRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedEnvelopeEvidenceAuthority {
    NativeRecord,
    CommonReducer,
    EngineControl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedEnvelopeEvidence {
    pub authority: ScopedEnvelopeEvidenceAuthority,
    pub quality: QualifiedValueQuality,
    pub effective_at: Option<QualifiedTimestamp>,
    pub completeness: ContractCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedNativeEvidenceWithheldReason {
    ProjectionBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedNativeEvidence {
    InlineSourceRecord {
        media_type: SourceMediaType,
        state: SourceRecordState,
        payload_hash: RecordHash,
        payload: Vec<u8>,
    },
    Withheld {
        media_type: SourceMediaType,
        state: SourceRecordState,
        payload_hash: RecordHash,
        reason: ScopedNativeEvidenceWithheldReason,
    },
    EngineControl,
}

/// Public-contract-shaped event payload. Internal object tokens, admission
/// ordinals, and duplicated envelope metadata are deliberately stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedObservationEvent {
    SourcePresence {
        change: ScopedAppendPresenceChange,
    },
    SourceReset {
        reset: ScopedAppendReset,
    },
    UsageV2 {
        fact_id: CanonicalFactId,
        operation: ScopedUsageV2Operation,
        retraction: Option<ScopedUsageV2RetractionCause>,
        revision: Box<UsageRevisionV2Fact>,
    },
    ObserverBootstrapComplete {
        barrier: Arc<ScopedBootstrapBarrier>,
    },
    ObserverResyncRequired {
        control: Arc<ScopedResyncRequired>,
    },
    ObserverResyncStarted {
        control: Arc<ScopedResyncStarted>,
    },
    ObserverResyncComplete {
        barrier: Arc<ScopedResyncBarrier>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedObservationEnvelope {
    pub contract_version: u32,
    pub observer_sequence: u64,
    pub scope_epoch: u64,
    pub event_id: ScopedObservationEventId,
    pub semantic_revision_ref: Option<SemanticRevisionRef>,
    pub root: ScopedObservationEnvelopeRoot,
    pub actor: ScopedActorRunRef,
    pub actor_attribution: ScopedActorAttribution,
    pub affiliations: ScopedActorAffiliationContext,
    pub source: ScopedObservationEnvelopeSource,
    pub native_time: Option<QualifiedTimestamp>,
    pub observed_at: i64,
    pub phase: ScopedAppendDeliveryPhase,
    pub evidence: ScopedEnvelopeEvidence,
    pub event: ScopedObservationEvent,
    pub native_evidence: ScopedNativeEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedEnvelopeError {
    #[error("scoped delivery metadata does not match its projected event")]
    DeliveryMismatch,
    #[error("scoped delivered source does not belong to the observer root")]
    RootSourceMismatch,
    #[error("scoped typed event does not belong to the observer root session")]
    RootSessionMismatch,
    #[error("scoped delivered source occurrence is malformed")]
    InvalidSourceOccurrence,
}

/// Immutable mapper created from the pre-access resolved root. It is safe to
/// retain with the event drain and cannot mint or revise observer identity.
#[derive(Debug, Clone)]
pub struct ScopedObservationEnvelopeMapper {
    root: ScopedObservationRootIdentity,
}

impl ScopedObservationEnvelopeMapper {
    fn new(root: ScopedObservationRootIdentity) -> Self {
        Self { root }
    }

    pub fn map(
        &self,
        delivered: ScopedDeliveredObservation,
    ) -> Result<ScopedObservationEnvelope, ScopedEnvelopeError> {
        if delivered.event_contract_version != SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
            || delivered.observer_sequence == 0
            || delivered.scope_epoch == 0
            || delivered.event_id != delivered.event.event_id()
            || delivered.semantic_revision_ref != delivered.event.semantic_revision_ref()
            || delivered.phase != delivered.event.phase()
            || &delivered.source != delivered.event.source()
        {
            return Err(ScopedEnvelopeError::DeliveryMismatch);
        }
        if !source_belongs_to_root(&delivered.source, &self.root) {
            return Err(ScopedEnvelopeError::RootSourceMismatch);
        }

        let contract_version = delivered.event_contract_version;
        let observer_sequence = delivered.observer_sequence;
        let scope_epoch = delivered.scope_epoch;
        let event_id = delivered.event_id;
        let semantic_revision_ref = delivered.semantic_revision_ref;
        let phase = delivered.phase;

        let mapped = match delivered.event {
            ScopedProjectedObservation::SourcePresence {
                observed_at,
                change,
                ..
            } => {
                if semantic_revision_ref.is_some() {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                let generation = match change {
                    ScopedAppendPresenceChange::Created { generation }
                    | ScopedAppendPresenceChange::Deleted { generation } => generation,
                };
                ScopedMappedEnvelopeParts {
                    actor_run_key: self.root.root_actor_run_key,
                    actor_attribution: ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::SourceLifecycleControl,
                    },
                    source: scoped_control_envelope_source(&delivered.source, generation),
                    native_time: None,
                    observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority: ScopedEnvelopeEvidenceAuthority::EngineControl,
                        quality: QualifiedValueQuality::Derived,
                        effective_at: None,
                        completeness: ContractCompleteness::Complete,
                    },
                    event: ScopedObservationEvent::SourcePresence { change },
                    native_evidence: ScopedNativeEvidence::EngineControl,
                }
            }
            ScopedProjectedObservation::SourceReset {
                observed_at, reset, ..
            } => {
                if semantic_revision_ref.is_some() {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                ScopedMappedEnvelopeParts {
                    actor_run_key: self.root.root_actor_run_key,
                    actor_attribution: ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::SourceLifecycleControl,
                    },
                    source: scoped_control_envelope_source(&delivered.source, reset.new_generation),
                    native_time: None,
                    observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority: ScopedEnvelopeEvidenceAuthority::EngineControl,
                        quality: QualifiedValueQuality::Derived,
                        effective_at: None,
                        completeness: ContractCompleteness::Complete,
                    },
                    event: ScopedObservationEvent::SourceReset { reset },
                    native_evidence: ScopedNativeEvidence::EngineControl,
                }
            }
            ScopedProjectedObservation::UsageV2 { event, .. } => {
                if semantic_revision_ref != Some(event.semantic_revision_ref) {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                if event.revision.session != self.root.session_key {
                    return Err(ScopedEnvelopeError::RootSessionMismatch);
                }
                let source = scoped_usage_envelope_source(&event.source)?;
                let native_time = event.revision.source_time.clone();
                let (authority, quality) = match event.operation {
                    ScopedUsageV2Operation::Upsert => (
                        ScopedEnvelopeEvidenceAuthority::NativeRecord,
                        QualifiedValueQuality::Exact,
                    ),
                    ScopedUsageV2Operation::Retract => (
                        ScopedEnvelopeEvidenceAuthority::CommonReducer,
                        QualifiedValueQuality::Derived,
                    ),
                };
                ScopedMappedEnvelopeParts {
                    actor_run_key: event.revision.actor_run,
                    actor_attribution: ScopedActorAttribution::DerivedExact,
                    source,
                    native_time: native_time.clone(),
                    observed_at: event.observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority,
                        quality,
                        effective_at: native_time,
                        completeness: ContractCompleteness::Complete,
                    },
                    event: ScopedObservationEvent::UsageV2 {
                        fact_id: event.fact_id,
                        operation: event.operation,
                        retraction: event.retraction,
                        revision: Box::new(event.revision),
                    },
                    native_evidence: ScopedNativeEvidence::Withheld {
                        media_type: event.source.media_type,
                        state: event.source.state,
                        payload_hash: event.source.payload_hash,
                        reason: ScopedNativeEvidenceWithheldReason::ProjectionBoundary,
                    },
                }
            }
            ScopedProjectedObservation::ObserverBootstrapComplete {
                observed_at,
                barrier,
                ..
            } => {
                if semantic_revision_ref.is_some()
                    || barrier.barrier_contract_version != SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION
                    || barrier.scope_epoch != scope_epoch
                    || barrier.root != self.root
                    || barrier.barrier_sequence != observer_sequence
                    || barrier.queue_state.scope_epoch != scope_epoch
                    || barrier.queue_state.offered_through_sequence < observer_sequence
                {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                ScopedMappedEnvelopeParts {
                    actor_run_key: self.root.root_actor_run_key,
                    actor_attribution: ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::ObserverLifecycleControl,
                    },
                    source: scoped_control_envelope_source(&delivered.source, scope_epoch),
                    native_time: None,
                    observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority: ScopedEnvelopeEvidenceAuthority::EngineControl,
                        quality: QualifiedValueQuality::Derived,
                        effective_at: None,
                        completeness: ContractCompleteness::Complete,
                    },
                    event: ScopedObservationEvent::ObserverBootstrapComplete { barrier },
                    native_evidence: ScopedNativeEvidence::EngineControl,
                }
            }
            ScopedProjectedObservation::ObserverResyncRequired {
                observed_at,
                control,
                ..
            } => {
                if semantic_revision_ref.is_some()
                    || control.root != self.root
                    || control.invalid_scope_epoch != scope_epoch
                    || control.control_sequence != observer_sequence
                    || control.last_contiguous_sequence >= observer_sequence
                {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                ScopedMappedEnvelopeParts {
                    actor_run_key: self.root.root_actor_run_key,
                    actor_attribution: ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::ObserverLifecycleControl,
                    },
                    source: scoped_control_envelope_source(&delivered.source, scope_epoch),
                    native_time: None,
                    observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority: ScopedEnvelopeEvidenceAuthority::EngineControl,
                        quality: QualifiedValueQuality::Derived,
                        effective_at: None,
                        completeness: ContractCompleteness::Complete,
                    },
                    event: ScopedObservationEvent::ObserverResyncRequired { control },
                    native_evidence: ScopedNativeEvidence::EngineControl,
                }
            }
            ScopedProjectedObservation::ObserverResyncStarted {
                observed_at,
                control,
                ..
            } => {
                if semantic_revision_ref.is_some()
                    || control.root != self.root
                    || control.new_scope_epoch != scope_epoch
                    || control.old_scope_epoch.checked_add(1) != Some(scope_epoch)
                    || control.control_sequence != observer_sequence
                    || control.required_control_sequence >= observer_sequence
                    || control.replacement != ScopedReplacementMode::FullSnapshot
                {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                ScopedMappedEnvelopeParts {
                    actor_run_key: self.root.root_actor_run_key,
                    actor_attribution: ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::ObserverLifecycleControl,
                    },
                    source: scoped_control_envelope_source(&delivered.source, scope_epoch),
                    native_time: None,
                    observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority: ScopedEnvelopeEvidenceAuthority::EngineControl,
                        quality: QualifiedValueQuality::Derived,
                        effective_at: None,
                        completeness: ContractCompleteness::Complete,
                    },
                    event: ScopedObservationEvent::ObserverResyncStarted { control },
                    native_evidence: ScopedNativeEvidence::EngineControl,
                }
            }
            ScopedProjectedObservation::ObserverResyncComplete {
                observed_at,
                barrier,
                ..
            } => {
                if semantic_revision_ref.is_some()
                    || barrier.barrier_contract_version != SCOPED_RESYNC_BARRIER_CONTRACT_VERSION
                    || barrier.root != self.root
                    || barrier.scope_epoch != scope_epoch
                    || barrier.replacement != ScopedReplacementMode::FullSnapshot
                    || barrier.barrier_sequence != observer_sequence
                    || barrier.started_control_sequence >= observer_sequence
                    || barrier.queue_state.scope_epoch != scope_epoch
                    || barrier.queue_state.offered_through_sequence < observer_sequence
                    || barrier.queue_state.continuity != ScopedObservationContinuity::Valid
                {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                ScopedMappedEnvelopeParts {
                    actor_run_key: self.root.root_actor_run_key,
                    actor_attribution: ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::ObserverLifecycleControl,
                    },
                    source: scoped_control_envelope_source(&delivered.source, scope_epoch),
                    native_time: None,
                    observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority: ScopedEnvelopeEvidenceAuthority::EngineControl,
                        quality: QualifiedValueQuality::Derived,
                        effective_at: None,
                        completeness: ContractCompleteness::Complete,
                    },
                    event: ScopedObservationEvent::ObserverResyncComplete { barrier },
                    native_evidence: ScopedNativeEvidence::EngineControl,
                }
            }
        };

        let actor = self.actor_ref(mapped.actor_run_key);
        let affiliations = ScopedActorAffiliationContext {
            actor_run_key: mapped.actor_run_key,
            team_key: None,
            native_team_id: None,
            team_name: None,
            member_key: None,
            workflow_key: None,
            native_workflow_id: None,
            completeness: ContractCompleteness::Unknown,
            derived_from_revision_refs: Vec::new(),
        };
        Ok(ScopedObservationEnvelope {
            contract_version,
            observer_sequence,
            scope_epoch,
            event_id,
            semantic_revision_ref,
            root: ScopedObservationEnvelopeRoot {
                session_ref: self.root.session_ref,
                session_key: self.root.session_key,
                native_session_claim: self.root.native_session_claim.clone(),
            },
            actor,
            actor_attribution: mapped.actor_attribution,
            affiliations,
            source: mapped.source,
            native_time: mapped.native_time,
            observed_at: mapped.observed_at,
            phase,
            evidence: mapped.evidence,
            event: mapped.event,
            native_evidence: mapped.native_evidence,
        })
    }

    fn actor_ref(&self, run_key: CanonicalEntityKey) -> ScopedActorRunRef {
        let native_session_id = self
            .root
            .native_session_claim
            .as_ref()
            .and_then(|claim| claim.identity.value.as_ref())
            .map(|identity| identity.native_id.clone());
        ScopedActorRunRef {
            root_session_key: self.root.session_key,
            run_key,
            role: if run_key == self.root.root_actor_run_key {
                ActorRunRole::Root
            } else {
                ActorRunRole::Child
            },
            parent_run_key: None,
            native_session_id,
            native_actor_id: None,
            native_actor_type: None,
        }
    }
}

struct ScopedMappedEnvelopeParts {
    actor_run_key: CanonicalEntityKey,
    actor_attribution: ScopedActorAttribution,
    source: ScopedObservationEnvelopeSource,
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    evidence: ScopedEnvelopeEvidence,
    event: ScopedObservationEvent,
    native_evidence: ScopedNativeEvidence,
}

fn scoped_control_envelope_source(
    source: &ScopedSourceObjectIdentity,
    generation: u64,
) -> ScopedObservationEnvelopeSource {
    ScopedObservationEnvelopeSource {
        instance_key: source.source_instance_key,
        stream_key: source.stream_key,
        object_key: source.object_key,
        locator_id: None,
        generation,
        source_record_id: None,
        record_index: None,
        cursor_start: None,
        cursor_end: None,
        byte_range: None,
    }
}

fn scoped_usage_envelope_source(
    source: &ScopedUsageV2Source,
) -> Result<ScopedObservationEnvelopeSource, ScopedEnvelopeError> {
    if source.provenance.generation == 0 {
        return Err(ScopedEnvelopeError::InvalidSourceOccurrence);
    }
    let byte_range = match (
        source.cursor_start.append_offset_value(),
        source.cursor_end.append_offset_value(),
    ) {
        (Some(start), Some(end)) if start <= end => Some(ScopedObservationByteRange { start, end }),
        (Some(_), Some(_)) => return Err(ScopedEnvelopeError::InvalidSourceOccurrence),
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => {
            return Err(ScopedEnvelopeError::InvalidSourceOccurrence);
        }
    };
    Ok(ScopedObservationEnvelopeSource {
        instance_key: source.object.source_instance_key,
        stream_key: source.object.stream_key,
        object_key: source.object.object_key,
        locator_id: None,
        generation: source.provenance.generation,
        source_record_id: Some(source.source_record_id),
        record_index: Some(source.ordinal_in_batch),
        cursor_start: Some(source.cursor_start.clone()),
        cursor_end: Some(source.cursor_end.clone()),
        byte_range,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationDeliveryState {
    pub scope_epoch: u64,
    pub offered_through_sequence: u64,
    pub delivered_through_sequence: u64,
    pub continuity: ScopedObservationContinuity,
    pub queued_semantic_events: usize,
    pub queued_retained_native_bytes: u64,
    pub queued_source_control_items: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedObservationContinuity {
    Bootstrap,
    Valid,
    ResyncRequired,
    Resyncing,
}

/// Store-free watermark substrate for common Decode coverage plus fact-family
/// domains that are simultaneously object-declared, contract-selected, and
/// reducer-supported. This remains crate-private and deliberately does not
/// masquerade as the complete RFC 012D watermark: scope coverage, actor/root
/// envelope state, and object-error lifecycle events still belong to the
/// future observer facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedObservationWatermarkCore {
    pub root: ScopedObservationRootIdentity,
    pub scope_epoch: u64,
    pub offered_through_sequence: u64,
    pub source_coverage: Vec<SourceCoverageSet>,
    pub explicit_object_errors: Vec<CoverageError>,
    pub queue_state: ScopedObservationDeliveryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedCoverageAssemblyError {
    #[error("scoped admission and coverage markers are not drained")]
    AdmissionNotDrained,
    #[error("scoped coverage has no observed source object")]
    NoObservedObject,
    #[error("scoped source coverage does not belong to the authorized adapter")]
    AdapterMismatch,
    #[error("scoped source coverage is missing a successfully offered object state")]
    ObjectNotOffered,
    #[error("scoped adapter is missing its promoted source declaration binding")]
    MissingSupportBinding,
    #[error("scoped coverage could not be represented by the common contract")]
    InvalidContract,
    #[error("scoped coverage belongs to an invalidated observer epoch")]
    ContinuityInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedDeliveryError {
    #[error("scoped observation delivery limits are invalid")]
    InvalidLimits,
    #[error("scoped semantic delivery batch exceeds absolute capacity")]
    SemanticBatchTooLarge,
    #[error("scoped semantic delivery queue is full")]
    SemanticQueueFull,
    #[error("scoped projected retained-native batch exceeds absolute capacity")]
    RetainedNativeBatchTooLarge,
    #[error("scoped projected retained-native queue is full")]
    RetainedNativeQueueFull,
    #[error("scoped source-control delivery batch exceeds absolute capacity")]
    SourceControlBatchTooLarge,
    #[error("scoped source-control delivery queue is full")]
    SourceControlQueueFull,
    #[error("scoped delivery accounting is exhausted")]
    CapacityExhausted,
    #[error("scoped observer delivery sequence is exhausted")]
    ObserverSequenceExhausted,
    #[error("scoped bootstrap delivery was already completed")]
    BootstrapAlreadyComplete,
    #[error("scoped observer requires a full resync before ordinary delivery")]
    ResyncRequired,
    #[error("scoped resync epoch accepts only correction-phase snapshot delivery")]
    InvalidResyncPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedBootstrapBarrierError {
    #[error("scoped bootstrap coverage is not ready: {0}")]
    Coverage(ScopedCoverageAssemblyError),
    #[error("scoped bootstrap control could not enter delivery: {0}")]
    Delivery(ScopedDeliveryError),
    #[error("scoped bootstrap state changed before its barrier could be offered")]
    StateChanged,
    #[error("scoped bootstrap barrier snapshot is invalid")]
    InvalidSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedContinuityError {
    #[error("scoped continuity cannot be invalidated before bootstrap completes")]
    BootstrapIncomplete,
    #[error("scoped continuity control does not belong to the bound root")]
    RootMismatch,
    #[error("scoped continuity control identity is invalid")]
    InvalidControlIdentity,
    #[error("scoped resync has not been required")]
    ResyncNotRequired,
    #[error("scoped resync cannot start before resync-required is delivered")]
    ResyncRequiredNotDelivered,
    #[error("scoped resync is already active and cannot be invalidated by this provisional path")]
    ResyncAlreadyActive,
    #[error("scoped epoch counter is exhausted")]
    EpochExhausted,
    #[error("scoped continuity control could not enter delivery: {0}")]
    Delivery(ScopedDeliveryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedProjectionDeliveryError {
    #[error("scoped semantic projection failed: {0}")]
    Projection(ScopedProjectionError),
    #[error("scoped projected delivery failed: {0}")]
    Delivery(ScopedDeliveryError),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ScopedDeliveryOfferFailure {
    pub error: ScopedDeliveryError,
    pub projected: Vec<ScopedProjectedObservation>,
}

struct QueuedProjectedObservation {
    observer_sequence: u64,
    retained_native_bytes: u64,
    value: ScopedProjectedObservation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ScopedProjectedMeasurement {
    semantic_events: usize,
    retained_native_bytes: u64,
    source_control_items: usize,
}

/// All-or-nothing projected delivery queue with separate semantic and
/// source-lifecycle capacity domains. Cross-lane order remains one total offer
/// order; capacity separation does not create two public streams.
pub struct ScopedObservationDeliveryLane {
    limits: ScopedObservationDeliveryLimits,
    scope_epoch: u64,
    semantic: VecDeque<QueuedProjectedObservation>,
    source_controls: VecDeque<QueuedProjectedObservation>,
    queued_retained_native_bytes: u64,
    next_observer_sequence: u64,
    delivered_through_sequence: u64,
    bootstrap_barrier: Option<Arc<ScopedBootstrapBarrier>>,
    resync_required: Option<Arc<ScopedResyncRequired>>,
    resync_started: Option<Arc<ScopedResyncStarted>>,
    resync_barrier: Option<Arc<ScopedResyncBarrier>>,
}

impl ScopedObservationDeliveryLane {
    pub fn new(limits: ScopedObservationDeliveryLimits) -> Result<Self, ScopedDeliveryError> {
        if limits.max_semantic_events == 0 || limits.max_source_control_items == 0 {
            return Err(ScopedDeliveryError::InvalidLimits);
        }
        Ok(Self {
            limits,
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            semantic: VecDeque::new(),
            source_controls: VecDeque::new(),
            queued_retained_native_bytes: 0,
            next_observer_sequence: 1,
            delivered_through_sequence: 0,
            bootstrap_barrier: None,
            resync_required: None,
            resync_started: None,
            resync_barrier: None,
        })
    }

    /// Test-only low-level delivery seam. Production composition offers only
    /// through `ScopedObservationAdmissionLane::offer_next` so reducer and
    /// admission state cannot advance independently of this queue.
    #[cfg(test)]
    pub fn offer(
        &mut self,
        projected: Vec<ScopedProjectedObservation>,
    ) -> Result<ScopedObservationOfferReceipt, ScopedDeliveryOfferFailure> {
        self.offer_projected(projected)
    }

    fn offer_projected(
        &mut self,
        projected: Vec<ScopedProjectedObservation>,
    ) -> Result<ScopedObservationOfferReceipt, ScopedDeliveryOfferFailure> {
        if self.resync_started.is_some()
            && projected
                .iter()
                .any(|value| value.phase() != ScopedAppendDeliveryPhase::Correction)
        {
            return Err(ScopedDeliveryOfferFailure {
                error: ScopedDeliveryError::InvalidResyncPhase,
                projected,
            });
        }
        if self.resync_required.is_some() && self.resync_started.is_none() {
            return Err(ScopedDeliveryOfferFailure {
                error: ScopedDeliveryError::ResyncRequired,
                projected,
            });
        }
        if self.bootstrap_barrier.is_some()
            && projected
                .iter()
                .any(|value| value.phase() == ScopedAppendDeliveryPhase::Bootstrap)
        {
            return Err(ScopedDeliveryOfferFailure {
                error: ScopedDeliveryError::BootstrapAlreadyComplete,
                projected,
            });
        }
        let measurement = match measure_projected_batch(&projected) {
            Ok(measurement) => measurement,
            Err(error) => return Err(ScopedDeliveryOfferFailure { error, projected }),
        };
        if let Err(error) = self.check_capacity(measurement) {
            return Err(ScopedDeliveryOfferFailure { error, projected });
        }
        let total_items = match measurement
            .semantic_events
            .checked_add(measurement.source_control_items)
        {
            Some(total) => total,
            None => {
                return Err(ScopedDeliveryOfferFailure {
                    error: ScopedDeliveryError::CapacityExhausted,
                    projected,
                });
            }
        };
        let total_items = match u64::try_from(total_items) {
            Ok(total) => total,
            Err(_) => {
                return Err(ScopedDeliveryOfferFailure {
                    error: ScopedDeliveryError::CapacityExhausted,
                    projected,
                });
            }
        };
        let after_sequence = match self.next_observer_sequence.checked_add(total_items) {
            Some(after) => after,
            None => {
                return Err(ScopedDeliveryOfferFailure {
                    error: ScopedDeliveryError::ObserverSequenceExhausted,
                    projected,
                });
            }
        };

        let first_offered_sequence = (!projected.is_empty()).then_some(self.next_observer_sequence);
        let mut observer_sequence = self.next_observer_sequence;
        for value in projected {
            let (semantic, retained_native_bytes) = projected_observation_measurement(&value);
            let queued = QueuedProjectedObservation {
                observer_sequence,
                retained_native_bytes,
                value,
            };
            if semantic {
                self.semantic.push_back(queued);
            } else {
                self.source_controls.push_back(queued);
            }
            observer_sequence += 1;
        }
        debug_assert_eq!(observer_sequence, after_sequence);
        self.next_observer_sequence = after_sequence;
        self.queued_retained_native_bytes += measurement.retained_native_bytes;

        Ok(ScopedObservationOfferReceipt {
            first_offered_sequence,
            offered_through_sequence: after_sequence
                .checked_sub(1)
                .expect("observer sequences begin at one"),
            semantic_events: measurement.semantic_events as u64,
            retained_native_bytes: measurement.retained_native_bytes,
            source_control_items: measurement.source_control_items as u64,
        })
    }

    fn offer_bootstrap_barrier(
        &mut self,
        root: &ScopedObservationRootIdentity,
        watermark: ScopedObservationWatermarkCore,
        root_present: bool,
        observed_at: i64,
    ) -> Result<Arc<ScopedBootstrapBarrier>, ScopedBootstrapBarrierError> {
        if self.resync_required.is_some() {
            return Err(ScopedBootstrapBarrierError::StateChanged);
        }
        if let Some(barrier) = &self.bootstrap_barrier {
            return if barrier.root == *root {
                Ok(Arc::clone(barrier))
            } else {
                Err(ScopedBootstrapBarrierError::StateChanged)
            };
        }
        let before = self.state();
        if watermark.root != *root
            || watermark.scope_epoch != SCOPED_INITIAL_SCOPE_EPOCH
            || watermark.scope_epoch != before.scope_epoch
            || watermark.offered_through_sequence != before.offered_through_sequence
            || watermark.queue_state != before
            || self
                .semantic
                .iter()
                .chain(self.source_controls.iter())
                .any(|queued| queued.value.phase() != ScopedAppendDeliveryPhase::Bootstrap)
        {
            return Err(ScopedBootstrapBarrierError::StateChanged);
        }
        let barrier_sequence = self.next_observer_sequence;
        let queued_source_control_items = before.queued_source_control_items.checked_add(1).ok_or(
            ScopedBootstrapBarrierError::Delivery(ScopedDeliveryError::CapacityExhausted),
        )?;
        let queue_state = ScopedObservationDeliveryState {
            scope_epoch: before.scope_epoch,
            offered_through_sequence: barrier_sequence,
            delivered_through_sequence: before.delivered_through_sequence,
            continuity: ScopedObservationContinuity::Valid,
            queued_semantic_events: before.queued_semantic_events,
            queued_retained_native_bytes: before.queued_retained_native_bytes,
            queued_source_control_items,
        };
        let snapshot_digest = bootstrap_snapshot_digest(
            root,
            root_present,
            &watermark.source_coverage,
            &watermark.explicit_object_errors,
        )?;
        let barrier = Arc::new(ScopedBootstrapBarrier {
            barrier_contract_version: SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION,
            root: root.clone(),
            scope_epoch: before.scope_epoch,
            barrier_sequence,
            snapshot_digest,
            source_coverage: watermark.source_coverage,
            explicit_object_errors: watermark.explicit_object_errors,
            queue_state,
            root_present,
        });
        let source = observer_control_source(root)
            .map_err(|()| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        let event_id = bootstrap_complete_event_id(root, before.scope_epoch, snapshot_digest);
        let receipt = self
            .offer_projected(vec![
                ScopedProjectedObservation::ObserverBootstrapComplete {
                    source,
                    observed_at,
                    event_id,
                    barrier: Arc::clone(&barrier),
                },
            ])
            .map_err(|failure| ScopedBootstrapBarrierError::Delivery(failure.error))?;
        debug_assert_eq!(receipt.first_offered_sequence, Some(barrier_sequence));
        debug_assert_eq!(receipt.offered_through_sequence, barrier_sequence);
        self.bootstrap_barrier = Some(Arc::clone(&barrier));
        debug_assert_eq!(self.state(), barrier.queue_state);
        Ok(barrier)
    }

    fn require_resync(
        &mut self,
        root: &ScopedObservationRootIdentity,
        reason: ScopedResyncReason,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncRequired>, ScopedContinuityError> {
        if self.resync_started.is_some() {
            return Err(ScopedContinuityError::ResyncAlreadyActive);
        }
        if let Some(control) = &self.resync_required {
            return if control.root == *root {
                Ok(Arc::clone(control))
            } else {
                Err(ScopedContinuityError::RootMismatch)
            };
        }
        let barrier = self
            .bootstrap_barrier
            .as_ref()
            .ok_or(ScopedContinuityError::BootstrapIncomplete)?;
        if barrier.root != *root {
            return Err(ScopedContinuityError::RootMismatch);
        }
        let baseline_snapshot_digest = self
            .resync_barrier
            .as_ref()
            .map_or(barrier.snapshot_digest, |value| {
                value.coverage_snapshot_digest
            });

        let discarded_semantic_events = u64::try_from(self.semantic.len())
            .map_err(|_| ScopedContinuityError::Delivery(ScopedDeliveryError::CapacityExhausted))?;
        let discarded_source_controls = u64::try_from(self.source_controls.len())
            .map_err(|_| ScopedContinuityError::Delivery(ScopedDeliveryError::CapacityExhausted))?;
        let control_sequence = self.next_observer_sequence;
        let after_sequence =
            control_sequence
                .checked_add(1)
                .ok_or(ScopedContinuityError::Delivery(
                    ScopedDeliveryError::ObserverSequenceExhausted,
                ))?;
        let source = observer_control_source(root)
            .map_err(|()| ScopedContinuityError::InvalidControlIdentity)?;
        let control = Arc::new(ScopedResyncRequired {
            root: root.clone(),
            invalid_scope_epoch: self.scope_epoch,
            control_sequence,
            last_contiguous_sequence: self.delivered_through_sequence,
            baseline_snapshot_digest,
            reason,
            discarded_semantic_events,
            discarded_source_controls,
            discarded_retained_native_bytes: self.queued_retained_native_bytes,
        });
        let event_id =
            resync_required_event_id(root, self.scope_epoch, reason, baseline_snapshot_digest);

        // Invalidation is the one operation allowed to bypass ordinary FIFO:
        // all not-yet-delivered old-epoch values are explicitly superseded by
        // this sticky control before it becomes visible.
        self.semantic.clear();
        self.source_controls.clear();
        self.queued_retained_native_bytes = 0;
        self.source_controls.push_back(QueuedProjectedObservation {
            observer_sequence: control_sequence,
            retained_native_bytes: 0,
            value: ScopedProjectedObservation::ObserverResyncRequired {
                source,
                observed_at,
                event_id,
                control: Arc::clone(&control),
            },
        });
        self.next_observer_sequence = after_sequence;
        self.resync_required = Some(Arc::clone(&control));
        debug_assert_eq!(
            self.state().continuity,
            ScopedObservationContinuity::ResyncRequired
        );
        Ok(control)
    }

    fn begin_resync(
        &mut self,
        root: &ScopedObservationRootIdentity,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncStarted>, ScopedContinuityError> {
        if let Some(control) = &self.resync_started {
            return if control.root == *root {
                Ok(Arc::clone(control))
            } else {
                Err(ScopedContinuityError::RootMismatch)
            };
        }
        let required = self
            .resync_required
            .as_ref()
            .ok_or(ScopedContinuityError::ResyncNotRequired)?;
        if required.root != *root {
            return Err(ScopedContinuityError::RootMismatch);
        }
        if self.delivered_through_sequence < required.control_sequence || !self.is_empty() {
            return Err(ScopedContinuityError::ResyncRequiredNotDelivered);
        }
        let new_scope_epoch = self
            .scope_epoch
            .checked_add(1)
            .ok_or(ScopedContinuityError::EpochExhausted)?;
        let control_sequence = self.next_observer_sequence;
        let after_sequence =
            control_sequence
                .checked_add(1)
                .ok_or(ScopedContinuityError::Delivery(
                    ScopedDeliveryError::ObserverSequenceExhausted,
                ))?;
        let source = observer_control_source(root)
            .map_err(|()| ScopedContinuityError::InvalidControlIdentity)?;
        let control = Arc::new(ScopedResyncStarted {
            root: root.clone(),
            old_scope_epoch: self.scope_epoch,
            new_scope_epoch,
            control_sequence,
            required_control_sequence: required.control_sequence,
            baseline_snapshot_digest: required.baseline_snapshot_digest,
            reason: required.reason,
            replacement: ScopedReplacementMode::FullSnapshot,
        });
        let event_id = resync_started_event_id(root, &control);

        self.scope_epoch = new_scope_epoch;
        self.source_controls.push_back(QueuedProjectedObservation {
            observer_sequence: control_sequence,
            retained_native_bytes: 0,
            value: ScopedProjectedObservation::ObserverResyncStarted {
                source,
                observed_at,
                event_id,
                control: Arc::clone(&control),
            },
        });
        self.next_observer_sequence = after_sequence;
        self.resync_started = Some(Arc::clone(&control));
        debug_assert_eq!(
            self.state().continuity,
            ScopedObservationContinuity::Resyncing
        );
        Ok(control)
    }

    fn offer_resync_barrier(
        &mut self,
        root: &ScopedObservationRootIdentity,
        watermark: ScopedObservationWatermarkCore,
        family_manifest: Vec<ScopedReplacementFamilyManifest>,
        expected_replacement_snapshot_digest: ScopedReplacementSnapshotDigest,
        root_present: bool,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncBarrier>, ScopedReplacementStageError> {
        let started = self
            .resync_started
            .as_ref()
            .ok_or(ScopedReplacementStageError::NotResyncing)?;
        if started.root != *root {
            return Err(ScopedReplacementStageError::RootMismatch);
        }
        let before = self.state();
        if before.continuity != ScopedObservationContinuity::Resyncing
            || watermark.root != *root
            || watermark.scope_epoch != before.scope_epoch
            || watermark.offered_through_sequence != before.offered_through_sequence
            || watermark.queue_state != before
            || started.new_scope_epoch != before.scope_epoch
            || self
                .semantic
                .iter()
                .chain(self.source_controls.iter())
                .any(|queued| queued.value.phase() != ScopedAppendDeliveryPhase::Correction)
        {
            return Err(ScopedReplacementStageError::StateChanged);
        }
        if replacement_snapshot_digest(
            root,
            root_present,
            &family_manifest,
            &watermark.source_coverage,
            &watermark.explicit_object_errors,
        )? != expected_replacement_snapshot_digest
        {
            return Err(ScopedReplacementStageError::InvalidManifest);
        }
        let barrier_sequence = self.next_observer_sequence;
        let queued_source_control_items = before
            .queued_source_control_items
            .checked_add(1)
            .ok_or(ScopedReplacementStageError::CapacityExhausted)?;
        let queue_state = ScopedObservationDeliveryState {
            scope_epoch: before.scope_epoch,
            offered_through_sequence: barrier_sequence,
            delivered_through_sequence: before.delivered_through_sequence,
            continuity: ScopedObservationContinuity::Valid,
            queued_semantic_events: before.queued_semantic_events,
            queued_retained_native_bytes: before.queued_retained_native_bytes,
            queued_source_control_items,
        };
        let coverage_snapshot_digest = bootstrap_snapshot_digest(
            root,
            root_present,
            &watermark.source_coverage,
            &watermark.explicit_object_errors,
        )
        .map_err(|_| ScopedReplacementStageError::InvalidManifest)?;
        let barrier = Arc::new(ScopedResyncBarrier {
            barrier_contract_version: SCOPED_RESYNC_BARRIER_CONTRACT_VERSION,
            root: root.clone(),
            scope_epoch: before.scope_epoch,
            replacement: ScopedReplacementMode::FullSnapshot,
            started_control_sequence: started.control_sequence,
            barrier_sequence,
            replacement_snapshot_digest: expected_replacement_snapshot_digest,
            coverage_snapshot_digest,
            family_manifest,
            source_coverage: watermark.source_coverage,
            explicit_object_errors: watermark.explicit_object_errors,
            queue_state,
            root_present,
        });
        let source = observer_control_source(root)
            .map_err(|()| ScopedReplacementStageError::InvalidManifest)?;
        let event_id = resync_complete_event_id(root, &barrier);
        let receipt = self
            .offer_projected(vec![ScopedProjectedObservation::ObserverResyncComplete {
                source,
                observed_at,
                event_id,
                barrier: Arc::clone(&barrier),
            }])
            .map_err(|failure| ScopedReplacementStageError::Delivery(failure.error))?;
        debug_assert_eq!(receipt.first_offered_sequence, Some(barrier_sequence));
        debug_assert_eq!(receipt.offered_through_sequence, barrier_sequence);

        self.resync_required = None;
        self.resync_started = None;
        self.resync_barrier = Some(Arc::clone(&barrier));
        debug_assert_eq!(self.state(), barrier.queue_state);
        Ok(barrier)
    }

    pub fn pop_next(&mut self) -> Option<ScopedDeliveredObservation> {
        let take_source_control = match (self.source_controls.front(), self.semantic.front()) {
            (Some(control), Some(semantic)) => {
                control.observer_sequence < semantic.observer_sequence
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return None,
        };
        let queued = if take_source_control {
            self.source_controls
                .pop_front()
                .expect("source-control front exists")
        } else {
            let queued = self.semantic.pop_front().expect("semantic front exists");
            self.queued_retained_native_bytes = self
                .queued_retained_native_bytes
                .checked_sub(queued.retained_native_bytes)
                .expect("queued projected retained-native accounting cannot underflow");
            queued
        };
        let event_id = queued.value.event_id();
        let semantic_revision_ref = queued.value.semantic_revision_ref();
        let phase = queued.value.phase();
        let source = queued.value.source().clone();
        debug_assert!(queued.observer_sequence > self.delivered_through_sequence);
        self.delivered_through_sequence = queued.observer_sequence;
        Some(ScopedDeliveredObservation {
            event_contract_version: SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION,
            observer_sequence: queued.observer_sequence,
            scope_epoch: self.scope_epoch,
            event_id,
            semantic_revision_ref,
            phase,
            source,
            event: queued.value,
        })
    }

    pub fn queued_semantic_events(&self) -> usize {
        self.semantic.len()
    }

    pub fn queued_retained_native_bytes(&self) -> u64 {
        self.queued_retained_native_bytes
    }

    pub fn queued_source_control_items(&self) -> usize {
        self.source_controls.len()
    }

    pub fn state(&self) -> ScopedObservationDeliveryState {
        ScopedObservationDeliveryState {
            scope_epoch: self.scope_epoch,
            offered_through_sequence: self.next_observer_sequence - 1,
            delivered_through_sequence: self.delivered_through_sequence,
            continuity: if self.resync_started.is_some() {
                ScopedObservationContinuity::Resyncing
            } else if self.resync_required.is_some() {
                ScopedObservationContinuity::ResyncRequired
            } else if self.bootstrap_barrier.is_some() {
                ScopedObservationContinuity::Valid
            } else {
                ScopedObservationContinuity::Bootstrap
            },
            queued_semantic_events: self.semantic.len(),
            queued_retained_native_bytes: self.queued_retained_native_bytes,
            queued_source_control_items: self.source_controls.len(),
        }
    }

    pub fn bootstrap_barrier(&self) -> Option<Arc<ScopedBootstrapBarrier>> {
        self.bootstrap_barrier.as_ref().map(Arc::clone)
    }

    pub fn resync_required(&self) -> Option<Arc<ScopedResyncRequired>> {
        self.resync_required.as_ref().map(Arc::clone)
    }

    pub fn resync_started(&self) -> Option<Arc<ScopedResyncStarted>> {
        self.resync_started.as_ref().map(Arc::clone)
    }

    pub fn resync_barrier(&self) -> Option<Arc<ScopedResyncBarrier>> {
        self.resync_barrier.as_ref().map(Arc::clone)
    }

    pub fn is_empty(&self) -> bool {
        self.semantic.is_empty() && self.source_controls.is_empty()
    }

    fn check_capacity(
        &self,
        measurement: ScopedProjectedMeasurement,
    ) -> Result<(), ScopedDeliveryError> {
        if measurement.semantic_events > self.limits.max_semantic_events {
            return Err(ScopedDeliveryError::SemanticBatchTooLarge);
        }
        if measurement.retained_native_bytes > self.limits.max_retained_native_bytes {
            return Err(ScopedDeliveryError::RetainedNativeBatchTooLarge);
        }
        if measurement.source_control_items > self.limits.max_source_control_items {
            return Err(ScopedDeliveryError::SourceControlBatchTooLarge);
        }
        if self
            .semantic
            .len()
            .checked_add(measurement.semantic_events)
            .is_none_or(|count| count > self.limits.max_semantic_events)
        {
            return Err(ScopedDeliveryError::SemanticQueueFull);
        }
        if self
            .queued_retained_native_bytes
            .checked_add(measurement.retained_native_bytes)
            .is_none_or(|bytes| bytes > self.limits.max_retained_native_bytes)
        {
            return Err(ScopedDeliveryError::RetainedNativeQueueFull);
        }
        if self
            .source_controls
            .len()
            .checked_add(measurement.source_control_items)
            .is_none_or(|count| count > self.limits.max_source_control_items)
        {
            return Err(ScopedDeliveryError::SourceControlQueueFull);
        }
        Ok(())
    }
}

fn measure_projected_batch(
    projected: &[ScopedProjectedObservation],
) -> Result<ScopedProjectedMeasurement, ScopedDeliveryError> {
    let mut measurement = ScopedProjectedMeasurement::default();
    for value in projected {
        let (semantic, retained_native_bytes) = projected_observation_measurement(value);
        if semantic {
            measurement.semantic_events = measurement
                .semantic_events
                .checked_add(1)
                .ok_or(ScopedDeliveryError::CapacityExhausted)?;
            measurement.retained_native_bytes = measurement
                .retained_native_bytes
                .checked_add(retained_native_bytes)
                .ok_or(ScopedDeliveryError::CapacityExhausted)?;
        } else {
            measurement.source_control_items = measurement
                .source_control_items
                .checked_add(1)
                .ok_or(ScopedDeliveryError::CapacityExhausted)?;
        }
    }
    Ok(measurement)
}

fn projected_observation_measurement(value: &ScopedProjectedObservation) -> (bool, u64) {
    match value {
        ScopedProjectedObservation::UsageV2 { .. } => (true, 0),
        ScopedProjectedObservation::SourcePresence { .. }
        | ScopedProjectedObservation::SourceReset { .. }
        | ScopedProjectedObservation::ObserverBootstrapComplete { .. }
        | ScopedProjectedObservation::ObserverResyncRequired { .. }
        | ScopedProjectedObservation::ObserverResyncStarted { .. }
        | ScopedProjectedObservation::ObserverResyncComplete { .. } => (false, 0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationProjectionLimits {
    pub max_usage_v2_entities: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedProjectionError {
    #[error("scoped observation projection limits are invalid")]
    InvalidLimits,
    #[error("scoped usage-v2 projection entity capacity is full")]
    UsageV2CapacityFull,
    #[error("scoped usage-v2 fact is missing its canonical semantic revision")]
    MissingSemanticRevision,
    #[error("scoped usage-v2 fact has an invalid canonical semantic revision")]
    InvalidSemanticRevision,
    #[error("scoped decoded fact provenance does not match its source record")]
    ProvenanceMismatch,
    #[error("scoped usage-v2 fact changed source ownership")]
    ConflictingOwnership,
    #[error("scoped usage-v2 correction did not advance its source cursor")]
    StaleRevision,
    #[error("scoped source reset does not match retained reducer generation")]
    InvalidResetState,
    #[error("scoped source presence change contradicts retained reducer state")]
    InvalidPresenceState,
    #[error("scoped replacement snapshot phase must be bootstrap or correction")]
    InvalidReplacementPhase,
    #[error("scoped replacement snapshot accounting is exhausted")]
    ReplacementCapacityExhausted,
    #[error("scoped projection reducer is not valid for this observer lifecycle")]
    InvalidLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedProjectionLifecycle {
    Active,
    Replacement { scope_epoch: u64 },
    Retired,
}

#[derive(Clone)]
struct ScopedUsageV2ProjectionState {
    object_token: u64,
    generation: u64,
    semantic: FactSemanticRevision,
    source: ScopedUsageV2Source,
    revision: UsageRevisionV2Fact,
}

struct ScopedProjectionPlan {
    projected: Vec<ScopedProjectedObservation>,
    mutation: ScopedProjectionMutation,
}

enum ScopedProjectionMutation {
    None,
    UpsertUsageV2(BTreeMap<CanonicalFactId, ScopedUsageV2ProjectionState>),
    RetractUsageV2(Vec<CanonicalFactId>),
}

#[derive(Clone, Copy)]
struct ScopedRetractionDelivery {
    lane_ordinal: u64,
    observed_at: i64,
    phase: ScopedAppendDeliveryPhase,
}

/// Database-free common reducer for typed scoped-observation facts.
///
/// Usage-v2 is the first family wired through this sink. Its state is bounded
/// by entity count, exact current repeats are silent, and event construction
/// happens only after the whole decoded record validates so a malformed fact
/// cannot partially mutate observer state.
pub struct ScopedObservationProjectionSink {
    limits: ScopedObservationProjectionLimits,
    lifecycle: ScopedProjectionLifecycle,
    usage_v2: BTreeMap<CanonicalFactId, ScopedUsageV2ProjectionState>,
}

impl ScopedObservationProjectionSink {
    pub fn new(limits: ScopedObservationProjectionLimits) -> Result<Self, ScopedProjectionError> {
        if limits.max_usage_v2_entities == 0 {
            return Err(ScopedProjectionError::InvalidLimits);
        }
        Ok(Self {
            limits,
            lifecycle: ScopedProjectionLifecycle::Active,
            usage_v2: BTreeMap::new(),
        })
    }

    fn new_replacement(
        limits: ScopedObservationProjectionLimits,
        scope_epoch: u64,
    ) -> Result<Self, ScopedProjectionError> {
        let mut projection = Self::new(limits)?;
        projection.lifecycle = ScopedProjectionLifecycle::Replacement { scope_epoch };
        Ok(projection)
    }

    #[cfg(test)]
    pub fn project(
        &mut self,
        frame: &ScopedQueuedObservationFrame,
    ) -> Result<Vec<ScopedProjectedObservation>, ScopedProjectionError> {
        let ScopedProjectionPlan {
            projected,
            mutation,
        } = self.prepare(frame)?;
        self.commit(mutation);
        Ok(projected)
    }

    fn prepare(
        &self,
        frame: &ScopedQueuedObservationFrame,
    ) -> Result<ScopedProjectionPlan, ScopedProjectionError> {
        match frame {
            ScopedQueuedObservationFrame::Presence {
                object_token,
                source,
                lane_ordinal,
                observed_at,
                phase,
                change,
            } => self.prepare_presence(
                *object_token,
                source,
                *lane_ordinal,
                *observed_at,
                *phase,
                *change,
            ),
            ScopedQueuedObservationFrame::Reset {
                object_token,
                source,
                lane_ordinal,
                observed_at,
                phase,
                reset,
            } => self.prepare_reset(
                *object_token,
                source,
                *lane_ordinal,
                *observed_at,
                *phase,
                *reset,
            ),
            ScopedQueuedObservationFrame::Decoded {
                object_token,
                source,
                lane_ordinal,
                phase,
                item,
            } => self.prepare_decoded(*object_token, source, *lane_ordinal, *phase, item),
        }
    }

    fn commit(&mut self, mutation: ScopedProjectionMutation) {
        match mutation {
            ScopedProjectionMutation::None => {}
            ScopedProjectionMutation::UpsertUsageV2(staged) => self.usage_v2.extend(staged),
            ScopedProjectionMutation::RetractUsageV2(fact_ids) => {
                for fact_id in fact_ids {
                    let removed = self.usage_v2.remove(&fact_id);
                    debug_assert!(removed.is_some(), "prepared usage-v2 retraction must exist");
                }
            }
        }
    }

    pub fn usage_v2_entity_count(&self) -> usize {
        self.usage_v2.len()
    }

    fn supports_coverage_domain(&self, domain: &CoverageDomain) -> bool {
        matches!(
            domain,
            CoverageDomain::FactFamily { family, version }
                if family == "runtime.usage-v2"
                    && *version == RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION
        )
    }

    pub fn usage_v2_revision(&self, fact_id: &CanonicalFactId) -> Option<SemanticRevisionRef> {
        self.usage_v2
            .get(fact_id)
            .map(|state| state.semantic.semantic_revision_ref)
    }

    pub fn usage_v2_replacement_snapshot(
        &self,
        phase: ScopedAppendDeliveryPhase,
    ) -> Result<ScopedUsageV2ReplacementSnapshot, ScopedProjectionError> {
        if phase == ScopedAppendDeliveryPhase::Live {
            return Err(ScopedProjectionError::InvalidReplacementPhase);
        }
        let entity_count = u64::try_from(self.usage_v2.len())
            .map_err(|_| ScopedProjectionError::ReplacementCapacityExhausted)?;
        let semantic_digest = usage_v2_replacement_digest(&self.usage_v2)?;
        let events = self
            .usage_v2
            .values()
            .map(|state| ScopedUsageV2Event {
                event_id: usage_v2_event_id(ScopedUsageV2Operation::Upsert, &state.semantic, None),
                semantic_revision_ref: state.semantic.semantic_revision_ref,
                fact_id: state.semantic.fact_id,
                operation: ScopedUsageV2Operation::Upsert,
                phase,
                observed_at: state.source.provenance.observed_at,
                source: state.source.clone(),
                retraction: None,
                revision: state.revision.clone(),
            })
            .collect();
        Ok(ScopedUsageV2ReplacementSnapshot {
            fact_family_contract_version: RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION,
            replacement_digest_contract_version: SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION,
            phase,
            entity_count,
            semantic_digest,
            events,
        })
    }

    fn prepare_reset(
        &self,
        object_token: u64,
        source: &ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        observed_at: i64,
        phase: ScopedAppendDeliveryPhase,
        reset: ScopedAppendReset,
    ) -> Result<ScopedProjectionPlan, ScopedProjectionError> {
        let (retracted, fact_ids) = self.prepare_usage_v2_retractions(
            object_token,
            reset.old_generation,
            ScopedRetractionDelivery {
                lane_ordinal,
                observed_at,
                phase: ScopedAppendDeliveryPhase::Correction,
            },
            ScopedUsageV2RetractionCause::Reset(reset),
            ScopedProjectionError::InvalidResetState,
        )?;
        let mut projected = Vec::with_capacity(retracted.len().saturating_add(1));
        projected.push(ScopedProjectedObservation::SourceReset {
            object_token,
            source: source.clone(),
            lane_ordinal,
            observed_at,
            phase,
            event_id: source_reset_event_id(source, reset),
            reset,
        });
        projected.extend(retracted);
        Ok(ScopedProjectionPlan {
            projected,
            mutation: ScopedProjectionMutation::RetractUsageV2(fact_ids),
        })
    }

    fn prepare_presence(
        &self,
        object_token: u64,
        source: &ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        observed_at: i64,
        phase: ScopedAppendDeliveryPhase,
        change: ScopedAppendPresenceChange,
    ) -> Result<ScopedProjectionPlan, ScopedProjectionError> {
        let (retracted, fact_ids) = match change {
            ScopedAppendPresenceChange::Created { .. } => {
                if self
                    .usage_v2
                    .values()
                    .any(|state| state.object_token == object_token)
                {
                    return Err(ScopedProjectionError::InvalidPresenceState);
                }
                (Vec::new(), Vec::new())
            }
            ScopedAppendPresenceChange::Deleted { generation } => self
                .prepare_usage_v2_retractions(
                    object_token,
                    generation,
                    ScopedRetractionDelivery {
                        lane_ordinal,
                        observed_at,
                        phase,
                    },
                    ScopedUsageV2RetractionCause::SourceDeleted { generation },
                    ScopedProjectionError::InvalidPresenceState,
                )?,
        };
        let mut projected = Vec::with_capacity(retracted.len().saturating_add(1));
        projected.push(ScopedProjectedObservation::SourcePresence {
            object_token,
            source: source.clone(),
            lane_ordinal,
            observed_at,
            phase,
            event_id: source_presence_event_id(source, change),
            change,
        });
        projected.extend(retracted);
        Ok(ScopedProjectionPlan {
            projected,
            mutation: if fact_ids.is_empty() {
                ScopedProjectionMutation::None
            } else {
                ScopedProjectionMutation::RetractUsageV2(fact_ids)
            },
        })
    }

    fn prepare_usage_v2_retractions(
        &self,
        object_token: u64,
        generation: u64,
        delivery: ScopedRetractionDelivery,
        cause: ScopedUsageV2RetractionCause,
        mismatch_error: ScopedProjectionError,
    ) -> Result<(Vec<ScopedProjectedObservation>, Vec<CanonicalFactId>), ScopedProjectionError>
    {
        if self
            .usage_v2
            .values()
            .any(|state| state.object_token == object_token && state.generation != generation)
        {
            return Err(mismatch_error);
        }
        let fact_ids = self
            .usage_v2
            .iter()
            .filter_map(|(fact_id, state)| {
                (state.object_token == object_token && state.generation == generation)
                    .then_some(*fact_id)
            })
            .collect::<Vec<_>>();
        let mut projected = Vec::with_capacity(fact_ids.len());
        for fact_id in &fact_ids {
            let state = self
                .usage_v2
                .get(fact_id)
                .expect("retraction keys came from the same reducer map");
            projected.push(ScopedProjectedObservation::UsageV2 {
                lane_ordinal: delivery.lane_ordinal,
                event: Box::new(ScopedUsageV2Event {
                    event_id: usage_v2_event_id(
                        ScopedUsageV2Operation::Retract,
                        &state.semantic,
                        Some(cause),
                    ),
                    semantic_revision_ref: state.semantic.semantic_revision_ref,
                    fact_id: state.semantic.fact_id,
                    operation: ScopedUsageV2Operation::Retract,
                    phase: delivery.phase,
                    observed_at: delivery.observed_at,
                    source: state.source.clone(),
                    retraction: Some(cause),
                    revision: state.revision.clone(),
                }),
            });
        }
        Ok((projected, fact_ids))
    }

    fn prepare_decoded(
        &self,
        object_token: u64,
        source: &ScopedSourceObjectIdentity,
        lane_ordinal: u64,
        phase: ScopedAppendDeliveryPhase,
        item: &ScopedDecodedAppendItem,
    ) -> Result<ScopedProjectionPlan, ScopedProjectionError> {
        let ScopedDecodedAppendItem::Record {
            evidence, batch, ..
        } = item
        else {
            return Ok(ScopedProjectionPlan {
                projected: Vec::new(),
                mutation: ScopedProjectionMutation::None,
            });
        };

        let mut staged = BTreeMap::<CanonicalFactId, ScopedUsageV2ProjectionState>::new();
        let mut projected = Vec::new();
        for envelope in batch.facts() {
            let Fact::UsageRevisionV2(revision) = &envelope.value else {
                continue;
            };
            let state = scoped_usage_v2_state(object_token, source, evidence, envelope, revision)?;
            let current = staged
                .get(&state.semantic.fact_id)
                .or_else(|| self.usage_v2.get(&state.semantic.fact_id));
            if let Some(current) = current {
                if current.semantic.fact_revision_id == state.semantic.fact_revision_id {
                    continue;
                }
                if current.object_token != state.object_token
                    || current.generation != state.generation
                {
                    return Err(ScopedProjectionError::ConflictingOwnership);
                }
                let old_cursor = current.source.cursor_end.append_offset_value();
                let next_cursor = state.source.cursor_end.append_offset_value();
                if old_cursor
                    .zip(next_cursor)
                    .is_none_or(|(old, next)| next <= old)
                {
                    return Err(ScopedProjectionError::StaleRevision);
                }
            }
            let event = ScopedUsageV2Event {
                event_id: usage_v2_event_id(ScopedUsageV2Operation::Upsert, &state.semantic, None),
                semantic_revision_ref: state.semantic.semantic_revision_ref,
                fact_id: state.semantic.fact_id,
                operation: ScopedUsageV2Operation::Upsert,
                phase,
                observed_at: state.source.provenance.observed_at,
                source: state.source.clone(),
                retraction: None,
                revision: state.revision.clone(),
            };
            projected.push(ScopedProjectedObservation::UsageV2 {
                lane_ordinal,
                event: Box::new(event),
            });
            staged.insert(state.semantic.fact_id, state);
        }

        let new_entities = staged
            .keys()
            .filter(|fact_id| !self.usage_v2.contains_key(fact_id))
            .count();
        if self
            .usage_v2
            .len()
            .checked_add(new_entities)
            .is_none_or(|count| count > self.limits.max_usage_v2_entities)
        {
            return Err(ScopedProjectionError::UsageV2CapacityFull);
        }
        Ok(ScopedProjectionPlan {
            projected,
            mutation: if staged.is_empty() {
                ScopedProjectionMutation::None
            } else {
                ScopedProjectionMutation::UpsertUsageV2(staged)
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedReplacementStageError {
    #[error("scoped replacement stage requires an active resync epoch")]
    NotResyncing,
    #[error("scoped replacement stage does not belong to the bound root")]
    RootMismatch,
    #[error("scoped replacement stage does not belong to the current epoch")]
    EpochMismatch,
    #[error("scoped replacement stage requires the current active reducer")]
    ActiveProjectionRequired,
    #[error("scoped replacement snapshot cannot freeze before admission drains")]
    AdmissionNotDrained,
    #[error("scoped replacement snapshot was already frozen")]
    SnapshotAlreadyPrepared,
    #[error("scoped replacement snapshot has not been frozen")]
    SnapshotNotPrepared,
    #[error("scoped replacement snapshot has not crossed the offered boundary")]
    SnapshotNotFullyOffered,
    #[error("scoped replacement snapshot accounting is exhausted")]
    CapacityExhausted,
    #[error("scoped replacement coverage is not ready: {0}")]
    Coverage(ScopedCoverageAssemblyError),
    #[error("scoped replacement family manifest is invalid")]
    InvalidManifest,
    #[error("scoped replacement state changed before completion could be offered")]
    StateChanged,
    #[error("scoped replacement reduction failed: {0}")]
    Projection(ScopedProjectionError),
    #[error("scoped replacement delivery failed: {0}")]
    Delivery(ScopedDeliveryError),
}

struct ScopedPreparedReplacementSnapshot {
    usage_v2: ScopedUsageV2ReplacementSnapshot,
    next_usage_event: usize,
}

/// Empty, epoch-bound reducer used to build a replacement without mutating
/// the reducer that still backs the consumer-visible old epoch. Native replay
/// is reduced silently; only the canonical latest-per-entity snapshot is
/// offered to the new epoch.
pub struct ScopedObservationReplacementStage {
    root: ScopedObservationRootIdentity,
    scope_epoch: u64,
    projection: ScopedObservationProjectionSink,
    prepared: Option<ScopedPreparedReplacementSnapshot>,
    completed: Option<Arc<ScopedResyncBarrier>>,
}

impl ScopedObservationReplacementStage {
    fn new(
        root: ScopedObservationRootIdentity,
        scope_epoch: u64,
        limits: ScopedObservationProjectionLimits,
    ) -> Result<Self, ScopedReplacementStageError> {
        Ok(Self {
            root,
            scope_epoch,
            projection: ScopedObservationProjectionSink::new_replacement(limits, scope_epoch)
                .map_err(ScopedReplacementStageError::Projection)?,
            prepared: None,
            completed: None,
        })
    }

    /// Reduce one admitted replay frame into the isolated replacement state.
    /// Replay input is normalized to Correction before semantic reduction and
    /// its incremental projected events are intentionally discarded.
    pub fn reduce_next(
        &mut self,
        admission: &mut ScopedObservationAdmissionLane,
    ) -> Result<bool, ScopedReplacementStageError> {
        if self.prepared.is_some() {
            return Err(ScopedReplacementStageError::SnapshotAlreadyPrepared);
        }
        if self.projection.lifecycle
            != (ScopedProjectionLifecycle::Replacement {
                scope_epoch: self.scope_epoch,
            })
        {
            return Err(ScopedReplacementStageError::EpochMismatch);
        }
        let Some(mut taken) = admission.take_next_frame() else {
            return Ok(false);
        };
        force_replacement_phase(&mut taken.frame);
        let plan = match self.projection.prepare(&taken.frame) {
            Ok(plan) => plan,
            Err(error) => {
                admission.restore_taken_frame(taken);
                return Err(ScopedReplacementStageError::Projection(error));
            }
        };
        debug_assert!(plan
            .projected
            .iter()
            .all(|value| value.phase() == ScopedAppendDeliveryPhase::Correction));
        self.projection.commit(plan.mutation);
        admission.commit_taken_frame(taken);
        Ok(true)
    }

    /// Freeze the canonical current replacement after the replay admission
    /// lane drains. Repeated freeze is rejected so a published prefix can
    /// never be paired with a silently changed reducer suffix.
    pub fn prepare_snapshot(
        &mut self,
        admission: &ScopedObservationAdmissionLane,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<&ScopedUsageV2ReplacementSnapshot, ScopedReplacementStageError> {
        self.validate_delivery(delivery)?;
        if self.prepared.is_some() {
            return Err(ScopedReplacementStageError::SnapshotAlreadyPrepared);
        }
        if !admission.is_empty() {
            return Err(ScopedReplacementStageError::AdmissionNotDrained);
        }
        let usage_v2 = self
            .projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .map_err(ScopedReplacementStageError::Projection)?;
        self.prepared = Some(ScopedPreparedReplacementSnapshot {
            usage_v2,
            next_usage_event: 0,
        });
        Ok(&self
            .prepared
            .as_ref()
            .expect("replacement snapshot was just installed")
            .usage_v2)
    }

    /// Offer one canonical snapshot entity. One-at-a-time publication remains
    /// bounded by the existing semantic queue and is retry-safe on pressure.
    pub fn offer_snapshot_next(
        &mut self,
        delivery: &mut ScopedObservationDeliveryLane,
    ) -> Result<Option<ScopedObservationOfferReceipt>, ScopedReplacementStageError> {
        self.validate_delivery(delivery)?;
        let prepared = self
            .prepared
            .as_mut()
            .ok_or(ScopedReplacementStageError::SnapshotNotPrepared)?;
        let Some(event) = prepared.usage_v2.events.get(prepared.next_usage_event) else {
            return Ok(None);
        };
        let lane_ordinal = u64::try_from(prepared.next_usage_event)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(ScopedReplacementStageError::CapacityExhausted)?;
        let receipt = delivery
            .offer_projected(vec![ScopedProjectedObservation::UsageV2 {
                lane_ordinal,
                event: Box::new(event.clone()),
            }])
            .map_err(|failure| ScopedReplacementStageError::Delivery(failure.error))?;
        prepared.next_usage_event += 1;
        Ok(Some(receipt))
    }

    pub fn snapshot_fully_offered(&self) -> bool {
        self.prepared
            .as_ref()
            .is_some_and(|prepared| prepared.next_usage_event == prepared.usage_v2.events.len())
    }

    pub fn usage_v2_entity_count(&self) -> usize {
        self.projection.usage_v2_entity_count()
    }

    fn family_manifest(
        &self,
        source_coverage: &[SourceCoverageSet],
    ) -> Result<Vec<ScopedReplacementFamilyManifest>, ScopedReplacementStageError> {
        let prepared = self
            .prepared
            .as_ref()
            .ok_or(ScopedReplacementStageError::SnapshotNotPrepared)?;
        let mut usage_completeness = None;
        for coverage in source_coverage {
            match &coverage.coverage_domain {
                CoverageDomain::Decode => {}
                CoverageDomain::FactFamily { family, version }
                    if family == "runtime.usage-v2"
                        && *version == RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION =>
                {
                    usage_completeness =
                        Some(usage_completeness.map_or(coverage.completeness, |current| {
                            merge_coverage_completeness(current, coverage.completeness)
                        }));
                }
                CoverageDomain::FactFamily { .. } | CoverageDomain::ProjectionPack { .. } => {
                    return Err(ScopedReplacementStageError::InvalidManifest);
                }
            }
        }
        let Some(completeness) = usage_completeness else {
            return if prepared.usage_v2.entity_count == 0 {
                Ok(Vec::new())
            } else {
                Err(ScopedReplacementStageError::InvalidManifest)
            };
        };
        Ok(vec![ScopedReplacementFamilyManifest {
            fact_family: "runtime.usage-v2".to_string(),
            contract_version: prepared.usage_v2.fact_family_contract_version,
            replacement_representation:
                ScopedReplacementRepresentation::UsageLatestContributionPerResponse,
            completeness,
            entity_or_event_count: prepared.usage_v2.entity_count,
            semantic_digest: prepared.usage_v2.semantic_digest,
        }])
    }

    fn validate_activation(
        &self,
        active: &ScopedObservationProjectionSink,
    ) -> Result<(), ScopedReplacementStageError> {
        if self.completed.is_some()
            || active.lifecycle != ScopedProjectionLifecycle::Active
            || active.limits != self.projection.limits
        {
            return Err(ScopedReplacementStageError::ActiveProjectionRequired);
        }
        if self.projection.lifecycle
            != (ScopedProjectionLifecycle::Replacement {
                scope_epoch: self.scope_epoch,
            })
        {
            return Err(ScopedReplacementStageError::EpochMismatch);
        }
        Ok(())
    }

    fn activate(
        &mut self,
        active: &mut ScopedObservationProjectionSink,
        barrier: Arc<ScopedResyncBarrier>,
    ) {
        active.lifecycle = ScopedProjectionLifecycle::Retired;
        self.projection.lifecycle = ScopedProjectionLifecycle::Active;
        std::mem::swap(active, &mut self.projection);
        self.completed = Some(barrier);
    }

    fn validate_delivery(
        &self,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<(), ScopedReplacementStageError> {
        let started = delivery
            .resync_started
            .as_ref()
            .ok_or(ScopedReplacementStageError::NotResyncing)?;
        if started.root != self.root {
            return Err(ScopedReplacementStageError::RootMismatch);
        }
        if delivery.scope_epoch != self.scope_epoch
            || started.new_scope_epoch != self.scope_epoch
            || delivery.state().continuity != ScopedObservationContinuity::Resyncing
        {
            return Err(ScopedReplacementStageError::EpochMismatch);
        }
        Ok(())
    }
}

fn force_replacement_phase(frame: &mut ScopedQueuedObservationFrame) {
    match frame {
        ScopedQueuedObservationFrame::Presence { phase, .. }
        | ScopedQueuedObservationFrame::Reset { phase, .. }
        | ScopedQueuedObservationFrame::Decoded { phase, .. } => {
            *phase = ScopedAppendDeliveryPhase::Correction;
        }
    }
}

fn scoped_usage_v2_state(
    object_token: u64,
    source: &ScopedSourceObjectIdentity,
    evidence: &ScopedDecodedRecordEvidence,
    envelope: &FactEnvelope,
    revision: &UsageRevisionV2Fact,
) -> Result<ScopedUsageV2ProjectionState, ScopedProjectionError> {
    if !fact_provenance_matches_evidence(&envelope.provenance, evidence) {
        return Err(ScopedProjectionError::ProvenanceMismatch);
    }
    revision
        .validate()
        .map_err(|_| ScopedProjectionError::InvalidSemanticRevision)?;
    let semantic = envelope
        .semantic_revision
        .ok_or(ScopedProjectionError::MissingSemanticRevision)?;
    if semantic.semantic_revision_ref.fact_revision_id != semantic.fact_revision_id {
        return Err(ScopedProjectionError::InvalidSemanticRevision);
    }
    let revision_key = revision
        .semantic_revision_key()
        .map_err(|_| ScopedProjectionError::InvalidSemanticRevision)?;
    let expected = FactRevisionId::derive(&semantic.fact_id, 1, &revision_key)
        .map_err(|_| ScopedProjectionError::InvalidSemanticRevision)?;
    if expected != semantic.fact_revision_id {
        return Err(ScopedProjectionError::InvalidSemanticRevision);
    }
    Ok(ScopedUsageV2ProjectionState {
        object_token,
        generation: evidence.generation,
        semantic,
        source: ScopedUsageV2Source {
            object: source.clone(),
            source_record_id: semantic.source_record_id,
            provenance: envelope.provenance.clone(),
            cursor_start: evidence.cursor_start.clone(),
            cursor_end: evidence.cursor_end.clone(),
            ordinal_in_batch: evidence.ordinal_in_batch,
            source_timestamp_hint: evidence.source_timestamp_hint,
            media_type: evidence.media_type.clone(),
            state: evidence.state,
            payload_hash: evidence.payload_hash,
        },
        revision: revision.clone(),
    })
}

fn fact_provenance_matches_evidence(
    provenance: &FactProvenance,
    evidence: &ScopedDecodedRecordEvidence,
) -> bool {
    provenance.source_instance_id == evidence.source_instance_id
        && provenance.stream_id == evidence.stream_id
        && provenance.object_id == evidence.object_id
        && provenance.generation == evidence.generation
        && provenance.cursor_start == evidence.cursor_start.as_bytes()
        && provenance.cursor_end == evidence.cursor_end.as_bytes()
        && provenance.record_hash == *evidence.payload_hash.as_bytes()
        && provenance.observed_at == evidence.observed_at
}

fn source_presence_event_id(
    source: &ScopedSourceObjectIdentity,
    change: ScopedAppendPresenceChange,
) -> ScopedObservationEventId {
    let mut hasher = source_control_event_hasher(source, b"source.presence");
    match change {
        ScopedAppendPresenceChange::Created { generation } => {
            hash_event_component(&mut hasher, b"created");
            hasher.update(&generation.to_be_bytes());
        }
        ScopedAppendPresenceChange::Deleted { generation } => {
            hash_event_component(&mut hasher, b"deleted");
            hasher.update(&generation.to_be_bytes());
        }
    }
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn source_reset_event_id(
    source: &ScopedSourceObjectIdentity,
    reset: ScopedAppendReset,
) -> ScopedObservationEventId {
    let mut hasher = source_control_event_hasher(source, b"source.reset");
    hasher.update(&reset.old_generation.to_be_bytes());
    hasher.update(&reset.new_generation.to_be_bytes());
    hasher.update(&[append_transition_tag(reset.reason)]);
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn observer_control_source(
    root: &ScopedObservationRootIdentity,
) -> Result<ScopedSourceObjectIdentity, ()> {
    let stream_key =
        CoverageStreamKey::derive(root.adapter_id.as_str(), b"spaghetti.observer-control")
            .map_err(|_| ())?;
    let object_key =
        CoverageObjectKey::derive("spaghetti.observer-control", root.session_key.as_bytes())
            .map_err(|_| ())?;
    Ok(ScopedSourceObjectIdentity {
        adapter_id: root.adapter_id.clone(),
        source_instance_key: root.source_instance_key,
        stream_key,
        object_key,
    })
}

fn bootstrap_snapshot_digest(
    root: &ScopedObservationRootIdentity,
    root_present: bool,
    source_coverage: &[SourceCoverageSet],
    explicit_object_errors: &[CoverageError],
) -> Result<ScopedBootstrapSnapshotDigest, ScopedBootstrapBarrierError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/bootstrap-snapshot-digest\0");
    hasher.update(&SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, root.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, root.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, root.session_key.as_bytes());
    hash_event_component(&mut hasher, root.root_actor_run_key.as_bytes());
    hasher.update(&[u8::from(root_present)]);
    hasher.update(&(source_coverage.len() as u64).to_be_bytes());
    for coverage in source_coverage {
        coverage
            .validate()
            .map_err(|_| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        let mut canonical = coverage.clone();
        for point in &mut canonical.points {
            point.provenance.observed_at = None;
        }
        let encoded = serde_json::to_vec(&canonical)
            .map_err(|_| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        hash_event_component(&mut hasher, &encoded);
    }
    hasher.update(&(explicit_object_errors.len() as u64).to_be_bytes());
    for error in explicit_object_errors {
        let encoded =
            serde_json::to_vec(error).map_err(|_| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        hash_event_component(&mut hasher, &encoded);
    }
    Ok(ScopedBootstrapSnapshotDigest(*hasher.finalize().as_bytes()))
}

fn replacement_snapshot_digest(
    root: &ScopedObservationRootIdentity,
    root_present: bool,
    family_manifest: &[ScopedReplacementFamilyManifest],
    source_coverage: &[SourceCoverageSet],
    explicit_object_errors: &[CoverageError],
) -> Result<ScopedReplacementSnapshotDigest, ScopedReplacementStageError> {
    if family_manifest.windows(2).any(|window| {
        (&window[0].fact_family, window[0].contract_version)
            >= (&window[1].fact_family, window[1].contract_version)
    }) {
        return Err(ScopedReplacementStageError::InvalidManifest);
    }
    let coverage_digest =
        bootstrap_snapshot_digest(root, root_present, source_coverage, explicit_object_errors)
            .map_err(|_| ScopedReplacementStageError::InvalidManifest)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/replacement-snapshot-digest\0");
    hasher.update(&SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, coverage_digest.as_bytes());
    hasher.update(&(family_manifest.len() as u64).to_be_bytes());
    for manifest in family_manifest {
        if manifest.fact_family.is_empty() || manifest.contract_version == 0 {
            return Err(ScopedReplacementStageError::InvalidManifest);
        }
        hash_event_component(&mut hasher, manifest.fact_family.as_bytes());
        hasher.update(&manifest.contract_version.to_be_bytes());
        hasher.update(&[match manifest.replacement_representation {
            ScopedReplacementRepresentation::UsageLatestContributionPerResponse => 1,
        }]);
        hasher.update(&[match manifest.completeness {
            CoverageSetCompleteness::Complete => 1,
            CoverageSetCompleteness::Partial => 2,
            CoverageSetCompleteness::Unavailable => 3,
        }]);
        hasher.update(&manifest.entity_or_event_count.to_be_bytes());
        hash_event_component(&mut hasher, manifest.semantic_digest.as_bytes());
    }
    Ok(ScopedReplacementSnapshotDigest(
        *hasher.finalize().as_bytes(),
    ))
}

fn bootstrap_complete_event_id(
    root: &ScopedObservationRootIdentity,
    scope_epoch: u64,
    snapshot_digest: ScopedBootstrapSnapshotDigest,
) -> ScopedObservationEventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"observer.bootstrap_complete");
    hash_event_component(&mut hasher, root.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, root.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, root.session_key.as_bytes());
    hasher.update(&scope_epoch.to_be_bytes());
    hash_event_component(&mut hasher, snapshot_digest.as_bytes());
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn resync_required_event_id(
    root: &ScopedObservationRootIdentity,
    scope_epoch: u64,
    reason: ScopedResyncReason,
    baseline_snapshot_digest: ScopedBootstrapSnapshotDigest,
) -> ScopedObservationEventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"observer.resync_required");
    hash_event_component(&mut hasher, root.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, root.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, root.session_key.as_bytes());
    hasher.update(&scope_epoch.to_be_bytes());
    hasher.update(&[match reason {
        ScopedResyncReason::WatcherOverflow => 1,
        ScopedResyncReason::TransportContinuityLoss => 2,
        ScopedResyncReason::ExplicitConsumerRequest => 3,
    }]);
    hash_event_component(&mut hasher, baseline_snapshot_digest.as_bytes());
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn resync_started_event_id(
    root: &ScopedObservationRootIdentity,
    control: &ScopedResyncStarted,
) -> ScopedObservationEventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"observer.resync_started");
    hash_event_component(&mut hasher, root.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, root.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, root.session_key.as_bytes());
    hasher.update(&control.old_scope_epoch.to_be_bytes());
    hasher.update(&control.new_scope_epoch.to_be_bytes());
    hasher.update(&[match control.reason {
        ScopedResyncReason::WatcherOverflow => 1,
        ScopedResyncReason::TransportContinuityLoss => 2,
        ScopedResyncReason::ExplicitConsumerRequest => 3,
    }]);
    hash_event_component(&mut hasher, control.baseline_snapshot_digest.as_bytes());
    hash_event_component(&mut hasher, b"full_snapshot");
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn resync_complete_event_id(
    root: &ScopedObservationRootIdentity,
    barrier: &ScopedResyncBarrier,
) -> ScopedObservationEventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"observer.resync_complete");
    hash_event_component(&mut hasher, root.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, root.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, root.session_key.as_bytes());
    hasher.update(&barrier.scope_epoch.to_be_bytes());
    hash_event_component(&mut hasher, barrier.replacement_snapshot_digest.as_bytes());
    hash_event_component(&mut hasher, b"full_snapshot");
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn source_control_event_hasher(
    source: &ScopedSourceObjectIdentity,
    event_kind: &[u8],
) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, event_kind);
    hash_event_component(&mut hasher, source.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, source.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, source.stream_key.as_bytes());
    hash_event_component(&mut hasher, source.object_key.as_bytes());
    hasher
}

fn usage_v2_event_id(
    operation: ScopedUsageV2Operation,
    semantic: &FactSemanticRevision,
    retraction: Option<ScopedUsageV2RetractionCause>,
) -> ScopedObservationEventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"runtime.usage-v2");
    hash_event_component(
        &mut hasher,
        match operation {
            ScopedUsageV2Operation::Upsert => b"upsert",
            ScopedUsageV2Operation::Retract => b"retract",
        },
    );
    hash_event_component(&mut hasher, semantic.fact_id.as_bytes());
    hasher.update(
        &semantic
            .semantic_revision_ref
            .semantic_reference_contract_version
            .to_be_bytes(),
    );
    hash_event_component(&mut hasher, semantic.fact_revision_id.as_bytes());
    hash_event_component(&mut hasher, semantic.source_record_id.as_bytes());
    match retraction {
        Some(ScopedUsageV2RetractionCause::Reset(reset)) => {
            hasher.update(&[1]);
            hasher.update(&reset.old_generation.to_be_bytes());
            hasher.update(&reset.new_generation.to_be_bytes());
            hasher.update(&[append_transition_tag(reset.reason)]);
        }
        Some(ScopedUsageV2RetractionCause::SourceDeleted { generation }) => {
            hasher.update(&[2]);
            hasher.update(&generation.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn usage_v2_replacement_digest(
    states: &BTreeMap<CanonicalFactId, ScopedUsageV2ProjectionState>,
) -> Result<ScopedReplacementSemanticDigest, ScopedProjectionError> {
    let entity_count = u64::try_from(states.len())
        .map_err(|_| ScopedProjectionError::ReplacementCapacityExhausted)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/replacement-semantic-digest\0");
    hasher.update(&SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"runtime.usage-v2");
    hasher.update(&RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    // BTreeMap iteration supplies canonical fact-ID order. The digest retains
    // topology-independent occurrence provenance while deliberately excluding
    // attachment phase, observer sequence, local numeric IDs, admission batch
    // ordinal, and observation time.
    for state in states.values() {
        let revision_key = state
            .revision
            .semantic_revision_key()
            .map_err(|_| ScopedProjectionError::InvalidSemanticRevision)?;
        hash_event_component(&mut hasher, state.semantic.fact_id.as_bytes());
        hasher.update(
            &state
                .semantic
                .semantic_revision_ref
                .semantic_reference_contract_version
                .to_be_bytes(),
        );
        hash_event_component(&mut hasher, state.semantic.fact_revision_id.as_bytes());
        hash_event_component(&mut hasher, state.semantic.source_record_id.as_bytes());
        hash_event_component(&mut hasher, &revision_key);
        hasher.update(&state.generation.to_be_bytes());
        hash_event_component(&mut hasher, state.source.cursor_start.as_bytes());
        hash_event_component(&mut hasher, state.source.cursor_end.as_bytes());
        hasher.update(state.source.payload_hash.as_bytes());
        hash_event_component(&mut hasher, state.source.media_type.as_str().as_bytes());
        hasher.update(&[match state.source.state {
            SourceRecordState::Present => 1,
            SourceRecordState::Absent => 2,
        }]);
    }
    Ok(ScopedReplacementSemanticDigest(
        *hasher.finalize().as_bytes(),
    ))
}

fn hash_event_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn append_transition_tag(transition: AppendTransition) -> u8 {
    match transition {
        AppendTransition::Initial => 1,
        AppendTransition::Continued => 2,
        AppendTransition::Truncated => 3,
        AppendTransition::IdentityChanged => 4,
        AppendTransition::PrefixMismatch => 5,
        AppendTransition::ContractReplay => 6,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedObjectRead {
    Available { bytes: Vec<u8>, revision: Revision },
    Unavailable,
    Oversized { observed_bytes: u64 },
    Unstable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedSourceFailureClass {
    InvalidConfiguration,
    InvalidCursor,
    PathEscape,
    LimitExceeded,
    Unstable,
    Database,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedDecodeFailureClass {
    Transient,
    RecordPermanent,
    StreamFatal,
    AdapterFatal,
    InvalidContract,
}

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationAccessError {
    #[error("scoped observation authorization failed: {0}")]
    Authorization(String),
    #[error("scoped observation root identity is invalid or inconsistent")]
    InvalidRootIdentity,
    #[error("invalid scoped access grant: {0}")]
    InvalidGrant(String),
    #[error("scoped observation host is closed")]
    Closed,
    #[error("a scoped access pass is already active")]
    PassAlreadyActive,
    #[error("scoped append bootstrap has not reached a drainable observation")]
    BootstrapNotDrained,
    #[error("scoped append bootstrap is already complete")]
    BootstrapAlreadyComplete,
    #[error("a scoped append observation is awaiting admission or discard")]
    ObservationPending,
    #[error("the scoped append observation does not match the pending admission")]
    ObservationNotPending,
    #[error("the scoped append observation has already completed decoding")]
    ObservationAlreadyDecoded,
    #[error("the scoped append observation has not completed decoding")]
    ObservationNotDecoded,
    #[error("the scoped append admission sequence is exhausted")]
    ObservationSequenceExhausted,
    #[error("invalid scoped decoder bounds")]
    InvalidDecodeBounds,
    #[error("invalid or duplicate scoped decoder fact-family coverage domain")]
    InvalidCoverageDomains,
    #[error("scoped decoder semantic source identity is invalid")]
    InvalidSemanticContext,
    #[error("scoped adapter decode failed: {0:?}")]
    Decode(ScopedDecodeFailureClass),
    #[error("scoped source access failed: {0:?}")]
    Source(ScopedSourceFailureClass),
    #[error(transparent)]
    Access(#[from] AccessBudgetError),
}

struct ScopedObservationAccessState {
    closed: AtomicBool,
    pass_active: AtomicBool,
}

/// The database-free scoped composition root owns the unforgeable typed
/// authorization and exact grants. It creates a fresh access ledger only at a
/// common-runtime pass boundary and never exposes the authorization itself.
pub struct ScopedObservationAccessHost {
    adapter: Arc<dyn AgentAdapter>,
    compatibility: CompatibilityDecision,
    authorization: TypedAccessAuthorization,
    root_identity: ScopedObservationRootIdentity,
    program_id: String,
    known_objects: Arc<BTreeMap<String, ScopedKnownObjectGrant>>,
    state: Arc<ScopedObservationAccessState>,
}

impl ScopedObservationAccessHost {
    pub fn authorize(
        registry: &AdapterRegistry,
        request: ScopedObservationAccessRequest,
    ) -> Result<Self, ScopedObservationAccessError> {
        let adapter_id = AdapterId::new(request.adapter_id.as_str())
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let adapter = registry.get(&adapter_id).cloned().ok_or_else(|| {
            ScopedObservationAccessError::Authorization(format!(
                "adapter {adapter_id} is not registered"
            ))
        })?;
        let (compatibility, authorization) = registry
            .authorize_typed_access(
                &adapter_id,
                &request.artifact_probe,
                SupportOperation::ScopedTypedObservation,
                &request.contract_request,
                &request.contract_offer,
            )
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let root_identity = request.root_identity.resolve(
            &adapter_id,
            authorization.contracts().external_entity_reference_version,
        )?;
        let program = authorization
            .select_scope_program(&request.program_id)
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program)?;
        let known_objects = validate_known_object_grants(&plan, request.known_objects)?;
        Ok(Self {
            adapter,
            compatibility,
            authorization,
            root_identity,
            program_id: request.program_id,
            known_objects: Arc::new(known_objects),
            state: Arc::new(ScopedObservationAccessState {
                closed: AtomicBool::new(false),
                pass_active: AtomicBool::new(false),
            }),
        })
    }

    pub fn compatibility(&self) -> &CompatibilityDecision {
        &self.compatibility
    }

    pub fn root_identity(&self) -> &ScopedObservationRootIdentity {
        &self.root_identity
    }

    pub fn envelope_mapper(&self) -> ScopedObservationEnvelopeMapper {
        ScopedObservationEnvelopeMapper::new(self.root_identity.clone())
    }

    /// Capture a self-consistent offered sequence plus eligible RFC 012A
    /// source/fact-family coverage after the admission lane has drained.
    /// Delivery backlog is allowed: offered and delivered are deliberately
    /// different boundaries.
    pub fn capture_watermark_core(
        &self,
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<ScopedObservationWatermarkCore, ScopedCoverageAssemblyError> {
        let queue_state = delivery.state();
        if queue_state.continuity == ScopedObservationContinuity::ResyncRequired {
            return Err(ScopedCoverageAssemblyError::ContinuityInvalid);
        }
        let source_coverage = assemble_scoped_coverage_sets(
            self.adapter.manifest(),
            self.authorization.contracts(),
            &self.root_identity,
            admission,
            projection,
        )?;
        let mut explicit_object_errors = source_coverage
            .iter()
            .flat_map(|set| set.explicit_errors.iter().cloned())
            .collect::<Vec<_>>();
        explicit_object_errors.sort();
        explicit_object_errors.dedup();
        Ok(ScopedObservationWatermarkCore {
            root: self.root_identity.clone(),
            scope_epoch: queue_state.scope_epoch,
            offered_through_sequence: queue_state.offered_through_sequence,
            source_coverage,
            explicit_object_errors,
            queue_state,
        })
    }

    /// Admit the epoch-1 bootstrap completion control after every source frame
    /// through the captured watermark has crossed the offered boundary. A
    /// successful barrier is retained by the delivery lane, so repeated
    /// `ready()`-style calls return the same value without redelivery.
    pub fn offer_bootstrap_complete(
        &self,
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        delivery: &mut ScopedObservationDeliveryLane,
        root_present: bool,
        observed_at: i64,
    ) -> Result<Arc<ScopedBootstrapBarrier>, ScopedBootstrapBarrierError> {
        if matches!(
            delivery.state().continuity,
            ScopedObservationContinuity::ResyncRequired | ScopedObservationContinuity::Resyncing
        ) {
            return Err(ScopedBootstrapBarrierError::StateChanged);
        }
        if let Some(barrier) = delivery.bootstrap_barrier() {
            return if barrier.root == self.root_identity {
                Ok(barrier)
            } else {
                Err(ScopedBootstrapBarrierError::StateChanged)
            };
        }
        let watermark = self
            .capture_watermark_core(admission, projection, delivery)
            .map_err(ScopedBootstrapBarrierError::Coverage)?;
        delivery.offer_bootstrap_barrier(&self.root_identity, watermark, root_present, observed_at)
    }

    /// Explicitly invalidate the current valid epoch. Ordinary queue pressure
    /// never calls this path; it represents independently detected semantic
    /// continuity loss and installs the sticky priority control.
    pub fn require_resync(
        &self,
        delivery: &mut ScopedObservationDeliveryLane,
        reason: ScopedResyncReason,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncRequired>, ScopedContinuityError> {
        delivery.require_resync(&self.root_identity, reason, observed_at)
    }

    /// Start a full replacement epoch after the sticky invalidation control
    /// has crossed the consumer-delivery boundary. The returned control is
    /// retained by the lane, making repeated calls attachment-idempotent.
    pub fn begin_resync(
        &self,
        delivery: &mut ScopedObservationDeliveryLane,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncStarted>, ScopedContinuityError> {
        delivery.begin_resync(&self.root_identity, observed_at)
    }

    /// Allocate the isolated empty reducer for the current replacement epoch.
    /// The active reducer supplies only its fixed capacity policy; none of its
    /// semantic state is cloned into replacement.
    pub fn open_resync_stage(
        &self,
        active: &ScopedObservationProjectionSink,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<ScopedObservationReplacementStage, ScopedReplacementStageError> {
        if active.lifecycle != ScopedProjectionLifecycle::Active {
            return Err(ScopedReplacementStageError::ActiveProjectionRequired);
        }
        let started = delivery
            .resync_started
            .as_ref()
            .ok_or(ScopedReplacementStageError::NotResyncing)?;
        if started.root != self.root_identity {
            return Err(ScopedReplacementStageError::RootMismatch);
        }
        if delivery.state().continuity != ScopedObservationContinuity::Resyncing
            || delivery.scope_epoch != started.new_scope_epoch
        {
            return Err(ScopedReplacementStageError::EpochMismatch);
        }
        ScopedObservationReplacementStage::new(
            self.root_identity.clone(),
            started.new_scope_epoch,
            active.limits,
        )
    }

    /// Validate the frozen per-family replacement against the exact offered
    /// coverage watermark, enqueue completion after every snapshot entity,
    /// then infallibly swap the isolated reducer into the active slot.
    pub fn offer_resync_complete(
        &self,
        active: &mut ScopedObservationProjectionSink,
        stage: &mut ScopedObservationReplacementStage,
        admission: &ScopedObservationAdmissionLane,
        delivery: &mut ScopedObservationDeliveryLane,
        root_present: bool,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncBarrier>, ScopedReplacementStageError> {
        if let Some(barrier) = &stage.completed {
            return if barrier.root == self.root_identity {
                Ok(Arc::clone(barrier))
            } else {
                Err(ScopedReplacementStageError::RootMismatch)
            };
        }
        stage.validate_delivery(delivery)?;
        stage.validate_activation(active)?;
        if stage.prepared.is_none() {
            return Err(ScopedReplacementStageError::SnapshotNotPrepared);
        }
        if !stage.snapshot_fully_offered() {
            return Err(ScopedReplacementStageError::SnapshotNotFullyOffered);
        }
        let watermark = self
            .capture_watermark_core(admission, &stage.projection, delivery)
            .map_err(ScopedReplacementStageError::Coverage)?;
        let family_manifest = stage.family_manifest(&watermark.source_coverage)?;
        let digest = replacement_snapshot_digest(
            &self.root_identity,
            root_present,
            &family_manifest,
            &watermark.source_coverage,
            &watermark.explicit_object_errors,
        )?;
        let barrier = delivery.offer_resync_barrier(
            &self.root_identity,
            watermark,
            family_manifest,
            digest,
            root_present,
            observed_at,
        )?;
        stage.activate(active, Arc::clone(&barrier));
        Ok(barrier)
    }

    /// Decode one already-read append observation through the exact adapter
    /// selected during authorization. Dependency access is fail-closed until
    /// scoped relation-backed `SourceAccess` composition lands.
    pub fn decode_append(
        &self,
        object: &mut ScopedKnownAppendObject,
        observation: &ScopedAppendObservation,
    ) -> Result<ScopedAppendDecodeOutcome, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        if !source_belongs_to_root(&object.source, &self.root_identity) {
            return Err(ScopedObservationAccessError::InvalidRootIdentity);
        }
        object.decode(
            self.adapter.as_ref(),
            observation,
            &ScopedDependencyAccessDenied,
        )
    }

    pub fn begin_pass(&self) -> Result<ScopedObservationAccessPass, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        self.state
            .pass_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ScopedObservationAccessError::PassAlreadyActive)?;
        if self.state.closed.load(Ordering::Acquire) {
            self.state.pass_active.store(false, Ordering::Release);
            return Err(ScopedObservationAccessError::Closed);
        }
        let plan = match self
            .authorization
            .select_scope_program(&self.program_id)
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))
            .and_then(|program| {
                AuthorizedScopeAccessPlan::from_authorized_program(program).map_err(Into::into)
            }) {
            Ok(plan) => plan,
            Err(error) => {
                self.state.pass_active.store(false, Ordering::Release);
                return Err(error);
            }
        };
        Ok(ScopedObservationAccessPass {
            plan,
            known_objects: Arc::clone(&self.known_objects),
            root_identity: self.root_identity.clone(),
            state: Arc::clone(&self.state),
            released: false,
        })
    }

    /// Idempotently prevent new passes. A pass read that observes closure after
    /// its operating-system call is accounted conservatively before returning.
    pub fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
    }

    pub fn is_closed(&self) -> bool {
        self.state.closed.load(Ordering::Acquire)
    }
}

impl Drop for ScopedObservationAccessHost {
    fn drop(&mut self) {
        self.close();
    }
}

/// One bounded reconciliation pass. Dropping the pass releases the host's
/// single-pass slot; a later pass receives a fresh declaration-sized ledger.
pub struct ScopedObservationAccessPass {
    plan: AuthorizedScopeAccessPlan,
    known_objects: Arc<BTreeMap<String, ScopedKnownObjectGrant>>,
    root_identity: ScopedObservationRootIdentity,
    state: Arc<ScopedObservationAccessState>,
    released: bool,
}

impl ScopedObservationAccessPass {
    fn validate_source_identity(
        &self,
        source: &ScopedSourceObjectIdentity,
    ) -> Result<(), ScopedObservationAccessError> {
        if source_belongs_to_root(source, &self.root_identity) {
            Ok(())
        } else {
            Err(ScopedObservationAccessError::InvalidRootIdentity)
        }
    }

    pub fn read_known_object(
        &self,
        request: ScopedKnownObjectReadRequest<'_>,
    ) -> Result<ScopedObjectRead, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        let grant = self.known_objects.get(request.relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} has no exact known-object grant",
                request.relation_id
            ))
        })?;
        let max_bytes = usize::try_from(request.max_bytes).map_err(|_| {
            ScopedObservationAccessError::InvalidGrant(
                "known-object byte reservation exceeds this platform".to_string(),
            )
        })?;
        let reservation = self.plan.reserve(ScopeAccessRequest {
            relation_id: request.relation_id,
            operation: AccessOperation::ObjectRead,
            phase: request.phase,
            parent_token: request.parent_token,
            identity_inputs: request.identity_inputs,
            depth: request.depth,
            max_bytes: request.max_bytes,
            max_rows: 0,
        })?;
        if reservation.primitive() != ScopeRelationPrimitive::KnownObject
            || reservation.access_root() != grant.access_root
            || reservation.locator() != grant.locator_id
        {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::InvalidGrant(
                "known-object grant no longer matches its authorized declaration".to_string(),
            ));
        }

        let read = match read_stable_file_confined(&grant.root, &grant.relative_path, max_bytes) {
            Ok(read) => read,
            Err(error) => {
                reservation.fail_conservative();
                return Err(ScopedObservationAccessError::Source(source_failure_class(
                    &error,
                )));
            }
        };
        if self.state.closed.load(Ordering::Acquire) {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::Closed);
        }
        match read {
            StableRead::Missing => {
                reservation.complete(0, 0, AccessOutcome::Unavailable)?;
                Ok(ScopedObjectRead::Unavailable)
            }
            StableRead::Oversized(stamp) => {
                reservation.complete(0, 0, AccessOutcome::Oversized)?;
                Ok(ScopedObjectRead::Oversized {
                    observed_bytes: stamp.len,
                })
            }
            StableRead::Unstable => {
                reservation.fail_conservative();
                Ok(ScopedObjectRead::Unstable)
            }
            StableRead::Stable {
                bytes, revision, ..
            } => {
                reservation.complete(bytes.len() as u64, 0, AccessOutcome::Available)?;
                Ok(ScopedObjectRead::Available { bytes, revision })
            }
        }
    }

    pub fn read_known_append(
        &self,
        driver: &AppendDelimitedFile,
        request: ScopedKnownAppendReadRequest<'_>,
    ) -> Result<AppendRead, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        let grant = self.known_objects.get(request.relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} has no exact known-object grant",
                request.relation_id
            ))
        })?;
        let reservation = self.plan.reserve(ScopeAccessRequest {
            relation_id: request.relation_id,
            operation: AccessOperation::ObjectRead,
            phase: request.phase,
            parent_token: request.parent_token,
            identity_inputs: request.identity_inputs,
            depth: request.depth,
            max_bytes: request.max_bytes,
            max_rows: 0,
        })?;
        if reservation.primitive() != ScopeRelationPrimitive::KnownObject
            || reservation.access_root() != grant.access_root
            || reservation.locator() != grant.locator_id
        {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::InvalidGrant(
                "known-object grant no longer matches its authorized declaration".to_string(),
            ));
        }

        let read = match driver.read_confined_bounded(
            &grant.root,
            &grant.relative_path,
            request.previous,
            request.origin,
            request.force_contract_replay,
            request.max_bytes,
        ) {
            Ok(read) => read,
            Err(error) => {
                reservation.fail_conservative();
                return Err(ScopedObservationAccessError::Source(source_failure_class(
                    &error,
                )));
            }
        };
        if self.state.closed.load(Ordering::Acquire) {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::Closed);
        }
        match &read {
            AppendRead::Missing => reservation.complete(0, 0, AccessOutcome::Unavailable)?,
            AppendRead::RetryTransient => reservation.fail_conservative(),
            AppendRead::Batch { bytes_read, .. } => {
                reservation.complete(*bytes_read, 0, AccessOutcome::Available)?;
            }
        }
        Ok(read)
    }

    pub fn report(&self) -> ScopeAccessReport {
        self.plan.report()
    }

    pub fn finish(mut self) -> ScopeAccessReport {
        let report = self.plan.report();
        self.release();
        report
    }

    fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.state.pass_active.store(false, Ordering::Release);
        }
    }
}

impl Drop for ScopedObservationAccessPass {
    fn drop(&mut self) {
        self.release();
    }
}

/// In-memory cursor/generation state for one exact append-delimited root. It
/// does not own a store, query service, watcher, or public event queue.
pub struct ScopedKnownAppendObject {
    object_token: u64,
    source: ScopedSourceObjectIdentity,
    driver: AppendDelimitedFile,
    decoder: ScopedAppendDecoderConfig,
    checkpoint: Option<AppendCheckpoint>,
    decoder_state: Option<Vec<u8>>,
    bootstrap_active: bool,
    bootstrap_observed: bool,
    bootstrap_blocked: bool,
    root_present: bool,
    next_admission_token: u64,
    pending: Option<PendingAppendState>,
}

struct PendingAppendState {
    admission_token: u64,
    checkpoint: Option<AppendCheckpoint>,
    bootstrap_blocked: bool,
    root_present: bool,
    staged_decoder_state: Option<Option<Vec<u8>>>,
}

impl ScopedKnownAppendObject {
    pub fn new(
        driver: AppendDelimitedFile,
        mut decoder: ScopedAppendDecoderConfig,
    ) -> Result<Self, ScopedObservationAccessError> {
        validate_decode_bounds(&decoder)?;
        let source = ScopedSourceObjectIdentity::from_semantic_context(&decoder.semantic_context)?;
        decoder.coverage_domains.sort();
        validate_scoped_coverage_domains(&source, &decoder.coverage_domains)?;
        let object_token = next_scoped_object_token()?;
        Ok(Self {
            object_token,
            source,
            driver,
            decoder,
            checkpoint: None,
            decoder_state: None,
            bootstrap_active: true,
            bootstrap_observed: false,
            bootstrap_blocked: false,
            root_present: false,
            next_admission_token: 1,
            pending: None,
        })
    }

    pub fn reconcile(
        &mut self,
        pass: &ScopedObservationAccessPass,
        request: ScopedAppendReconcileRequest<'_>,
    ) -> Result<ScopedAppendObservation, ScopedObservationAccessError> {
        if self.pending.is_some() {
            return Err(ScopedObservationAccessError::ObservationPending);
        }
        pass.validate_source_identity(&self.source)?;
        let previous_generation = self.checkpoint.as_ref().map(|value| value.generation);
        let read = pass.read_known_append(
            &self.driver,
            ScopedKnownAppendReadRequest {
                relation_id: request.relation_id,
                identity_inputs: request.identity_inputs,
                phase: request.access_phase,
                parent_token: request.parent_token,
                depth: request.depth,
                max_bytes: request.max_bytes,
                previous: self.checkpoint.as_ref(),
                origin: request.origin,
                force_contract_replay: request.force_contract_replay,
            },
        )?;
        let (
            reset_before_items,
            presence_change,
            became_missing,
            next_checkpoint,
            next_root_present,
            next_bootstrap_blocked,
        ) = match &read {
            AppendRead::Missing => {
                let became_missing = self.root_present;
                let presence_change = became_missing.then(|| ScopedAppendPresenceChange::Deleted {
                    generation: previous_generation
                        .expect("a present scoped append object owns a checkpoint generation"),
                });
                (
                    None,
                    presence_change,
                    became_missing,
                    self.checkpoint.clone(),
                    false,
                    false,
                )
            }
            AppendRead::RetryTransient => (
                None,
                None,
                false,
                self.checkpoint.clone(),
                self.root_present,
                true,
            ),
            AppendRead::Batch {
                checkpoint,
                transition,
                more_available,
                ..
            } => {
                let reset = if matches!(
                    transition,
                    AppendTransition::Truncated
                        | AppendTransition::IdentityChanged
                        | AppendTransition::PrefixMismatch
                        | AppendTransition::ContractReplay
                ) {
                    Some(ScopedAppendReset {
                        old_generation: previous_generation.expect(
                            "a reset transition is possible only with a previous checkpoint",
                        ),
                        new_generation: checkpoint.generation,
                        reason: *transition,
                    })
                } else {
                    None
                };
                let presence_change = (!self.root_present && self.bootstrap_observed).then_some(
                    ScopedAppendPresenceChange::Created {
                        generation: checkpoint.generation,
                    },
                );
                (
                    reset,
                    presence_change,
                    false,
                    Some(checkpoint.clone()),
                    true,
                    *more_available,
                )
            }
        };
        let phase = if reset_before_items.is_some() {
            ScopedAppendDeliveryPhase::Correction
        } else if self.bootstrap_active {
            ScopedAppendDeliveryPhase::Bootstrap
        } else {
            ScopedAppendDeliveryPhase::Live
        };
        let admission_token = self.next_admission_token;
        self.next_admission_token = self
            .next_admission_token
            .checked_add(1)
            .ok_or(ScopedObservationAccessError::ObservationSequenceExhausted)?;
        self.pending = Some(PendingAppendState {
            admission_token,
            checkpoint: next_checkpoint,
            bootstrap_blocked: next_bootstrap_blocked,
            root_present: next_root_present,
            staged_decoder_state: None,
        });
        Ok(ScopedAppendObservation {
            object_token: self.object_token,
            admission_token,
            observed_at: request.origin.observed_at,
            phase,
            reset_before_items,
            presence_change,
            root_present: next_root_present,
            became_missing,
            read,
        })
    }

    fn decode(
        &mut self,
        adapter: &dyn AgentAdapter,
        observation: &ScopedAppendObservation,
        source_access: &dyn SourceAccess,
    ) -> Result<ScopedAppendDecodeOutcome, ScopedObservationAccessError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(ScopedObservationAccessError::ObservationNotPending)?;
        if observation.object_token != self.object_token
            || pending.admission_token != observation.admission_token
        {
            return Err(ScopedObservationAccessError::ObservationNotPending);
        }
        if pending.staged_decoder_state.is_some() {
            return Err(ScopedObservationAccessError::ObservationAlreadyDecoded);
        }

        let mut next_decoder_state = if observation.reset_before_items.is_some() {
            None
        } else {
            self.decoder_state.clone()
        };
        let mut decoded_items = Vec::new();
        if let AppendRead::Batch { items, .. } = &observation.read {
            decoded_items.reserve(items.len());
            for item in items {
                match item {
                    AppendItem::Record(record) => {
                        let attempt = decode_record(DecodeRuntimeRequest {
                            adapter,
                            decoder: &self.decoder.decoder,
                            object_context: &self.decoder.object_context,
                            source_access,
                            record,
                            semantic_context: &self.decoder.semantic_context,
                            decoder_state: next_decoder_state.as_deref(),
                            retention: self.decoder.retention,
                            limits: DecodeRuntimeLimits {
                                max_facts: self.decoder.max_facts_per_record,
                                max_diagnostics: self.decoder.max_diagnostics_per_record,
                            },
                        });
                        let decoded = attempt.result.map_err(|error| {
                            ScopedObservationAccessError::Decode(decode_failure_class(&error))
                        })?;
                        if decoded.disposition == DecodeDisposition::RetryTransient {
                            return Ok(ScopedAppendDecodeOutcome::RetryTransient);
                        }
                        if let Some(state) = decoded.next_decoder_state.clone() {
                            next_decoder_state = Some(state);
                        }
                        decoded_items.push(ScopedDecodedAppendItem::Record {
                            evidence: Box::new(scoped_record_evidence(
                                record,
                                self.decoder.retention,
                            )),
                            disposition: decoded.disposition,
                            batch: decoded.batch,
                            quarantined: decoded.quarantined,
                        });
                    }
                    AppendItem::Quarantined(quarantine) => decoded_items.push(
                        ScopedDecodedAppendItem::DriverQuarantine(quarantine.clone()),
                    ),
                }
            }
        }

        let pending = self
            .pending
            .as_mut()
            .expect("pending observation remains owned during synchronous decode");
        pending.staged_decoder_state = Some(next_decoder_state);
        Ok(ScopedAppendDecodeOutcome::Ready(ScopedDecodedAppendBatch {
            object_token: observation.object_token,
            admission_token: observation.admission_token,
            items: decoded_items,
        }))
    }

    fn validate_admission(
        &self,
        observation: &ScopedAppendObservation,
        decoded: &ScopedDecodedAppendBatch,
    ) -> Result<(), ScopedObservationAccessError> {
        if observation.object_token != self.object_token
            || decoded.object_token != self.object_token
            || decoded.admission_token != observation.admission_token
        {
            return Err(ScopedObservationAccessError::ObservationNotPending);
        }
        let Some(pending) = self.pending.as_ref() else {
            return Err(ScopedObservationAccessError::ObservationNotPending);
        };
        if pending.admission_token != observation.admission_token {
            return Err(ScopedObservationAccessError::ObservationNotPending);
        }
        if pending.staged_decoder_state.is_none() {
            return Err(ScopedObservationAccessError::ObservationNotDecoded);
        }
        Ok(())
    }

    fn prepare_decode_coverage(
        &self,
        observation: &ScopedAppendObservation,
        decoded: &ScopedDecodedAppendBatch,
    ) -> Result<ScopedOfferedDecodeCoverage, ()> {
        let pending = self.pending.as_ref().ok_or(())?;
        if observation.object_token != self.object_token
            || decoded.object_token != self.object_token
            || pending.admission_token != observation.admission_token
            || decoded.admission_token != observation.admission_token
        {
            return Err(());
        }

        let error = |code: &str| CoverageError {
            stream_key: Some(self.source.stream_key),
            object_key: Some(self.source.object_key),
            code: code.to_string(),
        };
        match &observation.read {
            AppendRead::Missing => {
                let generation = pending
                    .checkpoint
                    .as_ref()
                    .map_or(1, |checkpoint| checkpoint.generation);
                Ok(ScopedOfferedDecodeCoverage {
                    source: self.source.clone(),
                    point: None,
                    explicit_absence_or_deletion: Some(CoverageAbsence {
                        stream_key: self.source.stream_key,
                        object_key: self.source.object_key,
                        generation,
                        kind: if self.checkpoint.is_some() {
                            CoverageAbsenceKind::Deleted
                        } else {
                            CoverageAbsenceKind::Absent
                        },
                    }),
                    explicit_errors: Vec::new(),
                    completeness: CoverageSetCompleteness::Complete,
                })
            }
            AppendRead::RetryTransient => {
                let generation = pending
                    .checkpoint
                    .as_ref()
                    .map_or(1, |checkpoint| checkpoint.generation);
                let position = pending
                    .checkpoint
                    .as_ref()
                    .map(scoped_append_coverage_position)
                    .transpose()?;
                let (status, completeness) = if position.is_some() {
                    (CoverageStatus::Partial, CoverageSetCompleteness::Partial)
                } else {
                    (
                        CoverageStatus::Unavailable {
                            reason: "source_retry_transient".to_string(),
                        },
                        CoverageSetCompleteness::Unavailable,
                    )
                };
                let point =
                    scoped_decode_coverage_point(&self.source, generation, position, status)?;
                Ok(ScopedOfferedDecodeCoverage {
                    source: self.source.clone(),
                    point: Some(point),
                    explicit_absence_or_deletion: None,
                    explicit_errors: vec![error("source_retry_transient")],
                    completeness,
                })
            }
            AppendRead::Batch {
                checkpoint,
                more_available,
                ..
            } => {
                let mut explicit_errors = Vec::new();
                let mut unavailable_reason = None;
                for item in &decoded.items {
                    match item {
                        ScopedDecodedAppendItem::DriverQuarantine(_) => {
                            unavailable_reason.get_or_insert("driver_quarantine");
                            explicit_errors.push(error("driver_quarantine"));
                        }
                        ScopedDecodedAppendItem::Record {
                            quarantined: true, ..
                        } => {
                            unavailable_reason.get_or_insert("decode_quarantine");
                            explicit_errors.push(error("decode_quarantine"));
                        }
                        ScopedDecodedAppendItem::Record {
                            quarantined: false, ..
                        } => {}
                    }
                }
                if *more_available {
                    explicit_errors.push(error("bounded_backlog"));
                }
                explicit_errors.sort();
                explicit_errors.dedup();

                let (status, completeness) = if let Some(reason) = unavailable_reason {
                    (
                        CoverageStatus::Unavailable {
                            reason: reason.to_string(),
                        },
                        CoverageSetCompleteness::Unavailable,
                    )
                } else if *more_available {
                    (CoverageStatus::Partial, CoverageSetCompleteness::Partial)
                } else {
                    (
                        CoverageStatus::CompleteThrough,
                        CoverageSetCompleteness::Complete,
                    )
                };
                let point = scoped_decode_coverage_point(
                    &self.source,
                    checkpoint.generation,
                    Some(scoped_append_coverage_position(checkpoint)?),
                    status,
                )?;
                Ok(ScopedOfferedDecodeCoverage {
                    source: self.source.clone(),
                    point: Some(point),
                    explicit_absence_or_deletion: None,
                    explicit_errors,
                    completeness,
                })
            }
        }
    }

    fn commit_admission(&mut self) {
        let pending = self
            .pending
            .take()
            .expect("validated scoped admission retains its pending state");
        let decoder_state = pending
            .staged_decoder_state
            .expect("validated scoped admission has staged decoder state");
        self.checkpoint = pending.checkpoint;
        self.decoder_state = decoder_state;
        self.bootstrap_blocked = pending.bootstrap_blocked;
        self.root_present = pending.root_present;
        self.bootstrap_observed = true;
    }

    /// Discard a read that could not be admitted. Its native access remains in
    /// the pass report, but the cursor does not advance and the next pass
    /// deterministically replays it.
    pub fn discard(
        &mut self,
        observation: &ScopedAppendObservation,
    ) -> Result<(), ScopedObservationAccessError> {
        let Some(pending) = self.pending.take() else {
            return Err(ScopedObservationAccessError::ObservationNotPending);
        };
        if observation.object_token != self.object_token
            || pending.admission_token != observation.admission_token
        {
            self.pending = Some(pending);
            return Err(ScopedObservationAccessError::ObservationNotPending);
        }
        Ok(())
    }

    /// Close the bootstrap admission phase after at least one stable missing
    /// or fully drained batch observation. An incomplete final record is safe:
    /// its checkpoint retains the suffix for the next live reconciliation.
    pub fn complete_bootstrap(&mut self) -> Result<(), ScopedObservationAccessError> {
        if !self.bootstrap_active {
            return Err(ScopedObservationAccessError::BootstrapAlreadyComplete);
        }
        if self.pending.is_some() || !self.bootstrap_observed || self.bootstrap_blocked {
            return Err(ScopedObservationAccessError::BootstrapNotDrained);
        }
        self.bootstrap_active = false;
        Ok(())
    }

    pub fn checkpoint(&self) -> Option<&AppendCheckpoint> {
        self.checkpoint.as_ref()
    }

    pub fn source_identity(&self) -> &ScopedSourceObjectIdentity {
        &self.source
    }

    fn coverage_membership_identity(&self) -> ScopedCoverageMembershipIdentity {
        ScopedCoverageMembershipIdentity {
            stream_key: Arc::from(self.decoder.semantic_context.stream_key()),
            object_key: Arc::from(self.decoder.semantic_context.object_key()),
            coverage_domains: self.decoder.coverage_domains.clone(),
        }
    }

    pub fn decoder_state(&self) -> Option<&[u8]> {
        self.decoder_state.as_deref()
    }

    pub fn root_present(&self) -> bool {
        self.root_present
    }

    pub fn bootstrap_active(&self) -> bool {
        self.bootstrap_active
    }
}

fn scoped_append_coverage_position(checkpoint: &AppendCheckpoint) -> Result<CoveragePosition, ()> {
    let cursor = checkpoint.cursor();
    CoveragePosition::derive(
        CoveragePositionKind::AppendCursor,
        cursor.as_bytes(),
        Some(checkpoint.committed_offset),
    )
    .map_err(|_| ())
}

fn scoped_decode_coverage_point(
    source: &ScopedSourceObjectIdentity,
    generation: u64,
    position: Option<CoveragePosition>,
    status: CoverageStatus,
) -> Result<SourceCoveragePoint, ()> {
    SourceCoveragePoint::new(
        CoverageDomain::Decode,
        source.adapter_id.as_str(),
        source.source_instance_key,
        source.stream_key,
        source.object_key,
        generation,
        position,
        status,
        CoverageProvenance::default(),
    )
    .map_err(|_| ())
}

fn assemble_scoped_coverage_sets(
    manifest: &crate::adapter::AdapterManifest,
    contracts: &crate::adapter::ContractVersionSelection,
    root: &ScopedObservationRootIdentity,
    admission: &ScopedObservationAdmissionLane,
    projection: &ScopedObservationProjectionSink,
) -> Result<Vec<SourceCoverageSet>, ScopedCoverageAssemblyError> {
    if !admission.is_empty() {
        return Err(ScopedCoverageAssemblyError::AdmissionNotDrained);
    }
    if admission.known_coverage_objects.is_empty() {
        return Err(ScopedCoverageAssemblyError::NoObservedObject);
    }
    let support = manifest
        .support_binding
        .as_ref()
        .ok_or(ScopedCoverageAssemblyError::MissingSupportBinding)?;
    let declaration_digest =
        CoverageDeclarationDigest::derive(support.source_declaration_digest().as_bytes())
            .map_err(|_| ScopedCoverageAssemblyError::InvalidContract)?;

    let mut by_domain_and_source = BTreeMap::<
        (CoverageDomain, CanonicalSourceInstanceKey),
        Vec<(
            &ScopedSourceObjectIdentity,
            &ScopedCoverageMembershipIdentity,
            &ScopedOfferedDecodeCoverage,
        )>,
    >::new();
    for (source, membership) in &admission.known_coverage_objects {
        if source.adapter_id != manifest.id
            || source.adapter_id != root.adapter_id
            || source.source_instance_key != root.source_instance_key
        {
            return Err(ScopedCoverageAssemblyError::AdapterMismatch);
        }
        let coverage = admission
            .offered_decode_coverage
            .get(source)
            .ok_or(ScopedCoverageAssemblyError::ObjectNotOffered)?;
        if &coverage.source != source {
            return Err(ScopedCoverageAssemblyError::InvalidContract);
        }
        by_domain_and_source
            .entry((CoverageDomain::Decode, source.source_instance_key))
            .or_default()
            .push((source, membership, coverage));
        for domain in &membership.coverage_domains {
            let CoverageDomain::FactFamily { family, version } = domain else {
                return Err(ScopedCoverageAssemblyError::InvalidContract);
            };
            if contracts.fact_family_versions.get(family) == Some(version)
                && projection.supports_coverage_domain(domain)
            {
                by_domain_and_source
                    .entry((domain.clone(), source.source_instance_key))
                    .or_default()
                    .push((source, membership, coverage));
            }
        }
    }

    let mut sets = Vec::with_capacity(by_domain_and_source.len());
    for ((domain, source_instance_key), mut objects) in by_domain_and_source {
        objects.sort_by(|left, right| {
            (
                left.1.stream_key.as_ref(),
                left.1.object_key.as_ref(),
                scoped_coverage_generation(left.2)
                    .map(|(generation, _)| generation)
                    .unwrap_or(0),
            )
                .cmp(&(
                    right.1.stream_key.as_ref(),
                    right.1.object_key.as_ref(),
                    scoped_coverage_generation(right.2)
                        .map(|(generation, _)| generation)
                        .unwrap_or(0),
                ))
        });
        let mut streams = objects
            .iter()
            .map(|(_, membership, _)| membership.stream_key.as_ref())
            .collect::<Vec<_>>();
        streams.sort_unstable();
        streams.dedup();
        let membership_objects = objects
            .iter()
            .map(|(_, membership, coverage)| {
                let (generation, absent) = scoped_coverage_generation(coverage)
                    .ok_or(ScopedCoverageAssemblyError::InvalidContract)?;
                Ok(CoverageMembershipObject {
                    stream_key: membership.stream_key.as_ref(),
                    object_key: membership.object_key.as_ref(),
                    generation,
                    absent,
                })
            })
            .collect::<Result<Vec<_>, ScopedCoverageAssemblyError>>()?;
        let membership_prefix = source_membership_prefix(&domain)
            .map_err(|_| ScopedCoverageAssemblyError::InvalidContract)?;
        let membership_revision =
            derive_coverage_membership_revision(&membership_prefix, &streams, &membership_objects)
                .map_err(|_| ScopedCoverageAssemblyError::InvalidContract)?;

        let mut points = Vec::new();
        let mut absences = Vec::new();
        let mut errors = Vec::new();
        let mut completeness = CoverageSetCompleteness::Complete;
        for (_, _, coverage) in objects {
            match (&coverage.point, &coverage.explicit_absence_or_deletion) {
                (Some(point), None) => {
                    points.push(scoped_coverage_point_for_domain(point, &domain)?)
                }
                (None, Some(absence)) => absences.push(absence.clone()),
                _ => return Err(ScopedCoverageAssemblyError::InvalidContract),
            }
            errors.extend(coverage.explicit_errors.iter().cloned());
            completeness = merge_coverage_completeness(completeness, coverage.completeness);
        }
        points.sort_by_key(|point| (point.stream_key, point.object_key, point.generation));
        absences.sort();
        errors.sort();
        errors.dedup();
        sets.push(
            SourceCoverageSet::new(
                domain,
                CoverageScope {
                    adapter_id: manifest.id.as_str().to_string(),
                    source_instance_key,
                    root_entity_key: Some(root.session_key),
                    support_release_id: support.support_release_id().to_string(),
                    source_or_scope_declaration_digest: declaration_digest,
                },
                membership_revision,
                points,
                absences,
                errors,
                completeness,
            )
            .map_err(|_| ScopedCoverageAssemblyError::InvalidContract)?,
        );
    }
    Ok(sets)
}

fn scoped_coverage_point_for_domain(
    point: &SourceCoveragePoint,
    domain: &CoverageDomain,
) -> Result<SourceCoveragePoint, ScopedCoverageAssemblyError> {
    SourceCoveragePoint::new(
        domain.clone(),
        point.adapter_id.clone(),
        point.source_instance_key,
        point.stream_key,
        point.object_key,
        point.generation,
        point.position.clone(),
        point.status.clone(),
        point.provenance.clone(),
    )
    .map_err(|_| ScopedCoverageAssemblyError::InvalidContract)
}

fn scoped_coverage_generation(coverage: &ScopedOfferedDecodeCoverage) -> Option<(u64, bool)> {
    match (&coverage.point, &coverage.explicit_absence_or_deletion) {
        (Some(point), None) => Some((point.generation, false)),
        (None, Some(absence)) => Some((absence.generation, true)),
        _ => None,
    }
}

fn merge_coverage_completeness(
    left: CoverageSetCompleteness,
    right: CoverageSetCompleteness,
) -> CoverageSetCompleteness {
    match (left, right) {
        (CoverageSetCompleteness::Unavailable, _) | (_, CoverageSetCompleteness::Unavailable) => {
            CoverageSetCompleteness::Unavailable
        }
        (CoverageSetCompleteness::Partial, _) | (_, CoverageSetCompleteness::Partial) => {
            CoverageSetCompleteness::Partial
        }
        (CoverageSetCompleteness::Complete, CoverageSetCompleteness::Complete) => {
            CoverageSetCompleteness::Complete
        }
    }
}

fn validate_known_object_grants(
    plan: &AuthorizedScopeAccessPlan,
    grants: Vec<ScopedKnownObjectGrant>,
) -> Result<BTreeMap<String, ScopedKnownObjectGrant>, ScopedObservationAccessError> {
    if grants.is_empty() {
        return Err(ScopedObservationAccessError::InvalidGrant(
            "at least one exact known-object grant is required".to_string(),
        ));
    }
    let mut validated = BTreeMap::new();
    for grant in grants {
        if grant.root.as_os_str().is_empty() || !grant.root.is_absolute() {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} requires an absolute host-approved root",
                grant.relation_id
            )));
        }
        confined_relative_path_key(&grant.relative_path).map_err(|_| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} has a non-confined relative locator",
                grant.relation_id
            ))
        })?;
        let relation = plan.relation(&grant.relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} is absent from the authorized program",
                grant.relation_id
            ))
        })?;
        if relation.primitive != ScopeRelationPrimitive::KnownObject
            || relation.access_root != grant.access_root
            || relation.locator != grant.locator_id
        {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} does not match an exact authorized known object",
                grant.relation_id
            )));
        }
        let relation_id = grant.relation_id.clone();
        if validated.insert(relation_id.clone(), grant).is_some() {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "duplicate known-object grant for relation {relation_id:?}"
            )));
        }
    }
    Ok(validated)
}

fn source_failure_class(error: &SourceDriverError) -> ScopedSourceFailureClass {
    match error {
        SourceDriverError::InvalidConfig(_) => ScopedSourceFailureClass::InvalidConfiguration,
        SourceDriverError::InvalidCursor(_) => ScopedSourceFailureClass::InvalidCursor,
        SourceDriverError::PathEscape(_) => ScopedSourceFailureClass::PathEscape,
        SourceDriverError::LimitExceeded(_) => ScopedSourceFailureClass::LimitExceeded,
        SourceDriverError::Unstable(_) => ScopedSourceFailureClass::Unstable,
        SourceDriverError::Database(_) => ScopedSourceFailureClass::Database,
        SourceDriverError::Io { .. } => ScopedSourceFailureClass::Io,
    }
}

fn decode_failure_class(error: &AdapterError) -> ScopedDecodeFailureClass {
    match error.class {
        AdapterErrorClass::Transient => ScopedDecodeFailureClass::Transient,
        AdapterErrorClass::RecordPermanent => ScopedDecodeFailureClass::RecordPermanent,
        AdapterErrorClass::StreamFatal => ScopedDecodeFailureClass::StreamFatal,
        AdapterErrorClass::AdapterFatal => ScopedDecodeFailureClass::AdapterFatal,
        AdapterErrorClass::InvalidContract => ScopedDecodeFailureClass::InvalidContract,
    }
}

fn admission_validation_error(error: ScopedObservationAccessError) -> ScopedAdmissionError {
    match error {
        ScopedObservationAccessError::ObservationNotDecoded => {
            ScopedAdmissionError::ObservationNotDecoded
        }
        _ => ScopedAdmissionError::ObservationMismatch,
    }
}

fn decoded_item_measurement(item: &ScopedDecodedAppendItem) -> Option<(u64, u64)> {
    match item {
        ScopedDecodedAppendItem::DriverQuarantine(_) => Some((1, 0)),
        ScopedDecodedAppendItem::Record {
            evidence, batch, ..
        } => {
            let semantic_items = batch
                .facts()
                .len()
                .checked_add(batch.diagnostics().len())?
                .max(1);
            let data_events = u64::try_from(semantic_items).ok()?;
            let mut retained_native_bytes = match &evidence.retained_payload {
                Some(payload) => u64::try_from(payload.len()).ok()?,
                None => 0,
            };
            for fact in batch.facts() {
                if let Fact::UnknownRecord { raw_payload, .. } = &fact.value {
                    retained_native_bytes = retained_native_bytes
                        .checked_add(u64::try_from(raw_payload.len()).ok()?)?;
                }
            }
            Some((data_events, retained_native_bytes))
        }
    }
}

static NEXT_SCOPED_OBJECT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn next_scoped_object_token() -> Result<u64, ScopedObservationAccessError> {
    NEXT_SCOPED_OBJECT_TOKEN
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| ScopedObservationAccessError::ObservationSequenceExhausted)
}

fn validate_decode_bounds(
    decoder: &ScopedAppendDecoderConfig,
) -> Result<(), ScopedObservationAccessError> {
    const MAX_FACTS_PER_RECORD: usize = 8_192;
    const MAX_DIAGNOSTICS_PER_RECORD: usize = 256;
    if decoder.max_facts_per_record == 0
        || decoder.max_facts_per_record > MAX_FACTS_PER_RECORD
        || decoder.max_diagnostics_per_record == 0
        || decoder.max_diagnostics_per_record > MAX_DIAGNOSTICS_PER_RECORD
    {
        return Err(ScopedObservationAccessError::InvalidDecodeBounds);
    }
    Ok(())
}

fn validate_scoped_coverage_domains(
    source: &ScopedSourceObjectIdentity,
    domains: &[CoverageDomain],
) -> Result<(), ScopedObservationAccessError> {
    if domains.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ScopedObservationAccessError::InvalidCoverageDomains);
    }
    for domain in domains {
        if !matches!(domain, CoverageDomain::FactFamily { .. })
            || SourceCoveragePoint::new(
                domain.clone(),
                source.adapter_id.as_str(),
                source.source_instance_key,
                source.stream_key,
                source.object_key,
                1,
                None,
                CoverageStatus::Partial,
                CoverageProvenance::default(),
            )
            .is_err()
        {
            return Err(ScopedObservationAccessError::InvalidCoverageDomains);
        }
    }
    Ok(())
}

fn scoped_record_evidence(
    record: &SourceRecord,
    retention: RawRetentionPolicy,
) -> ScopedDecodedRecordEvidence {
    let retained_payload = match retention {
        RawRetentionPolicy::None | RawRetentionPolicy::HashOnly => None,
        RawRetentionPolicy::DiagnosticExcerpt => Some(diagnostic_excerpt(&record.payload)),
        RawRetentionPolicy::Full => Some(record.payload.clone()),
    };
    ScopedDecodedRecordEvidence {
        source_instance_id: record.source_instance_id,
        stream_id: record.stream_id,
        object_id: record.object_id,
        generation: record.generation,
        cursor_start: record.cursor_start.clone(),
        cursor_end: record.cursor_end.clone(),
        ordinal_in_batch: record.ordinal_in_batch,
        observed_at: record.observed_at,
        source_timestamp_hint: record.source_timestamp_hint,
        media_type: record.media_type.clone(),
        state: record.state,
        payload_hash: record.payload_hash,
        retained_payload,
    }
}

struct ScopedDependencyAccessDenied;

impl SourceAccess for ScopedDependencyAccessDenied {
    fn read_object(
        &self,
        _root_name: &str,
        _relative_path: &std::path::Path,
        _max_bytes: usize,
    ) -> Result<SourceSnapshot, AdapterError> {
        Err(scoped_dependency_access_error())
    }

    fn query_source_db(&self, _query: &SourceQuery) -> Result<SourceRows, AdapterError> {
        Err(scoped_dependency_access_error())
    }

    fn list_objects(
        &self,
        _request: &SourceObjectListRequest,
    ) -> Result<SourceObjectList, AdapterError> {
        Err(scoped_dependency_access_error())
    }
}

fn scoped_dependency_access_error() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::InvalidContract,
        "scoped_dependency_access_undeclared",
        "decoder requested dependency access without a scoped relation-backed grant",
    )
}

#[cfg(test)]
mod projection_tests {
    use crate::adapter::{
        ContractCompleteness, NativeIdentity, QualifiedTimestamp, QualifiedValue,
        QualifiedValueQuality, TimestampQuality, UsageBucketsV2, UsageQualifiedValue,
        UsageResponseIdentity, UsageValueAuthority, UsageValueProvenance,
    };
    use crate::source::RecordOrigin;

    use super::*;

    const OBJECT_TOKEN: u64 = 41;

    fn semantic_context() -> FactSemanticContext {
        FactSemanticContext::new(
            &AdapterId::new("fixture").unwrap(),
            1,
            b"stable-source-instance",
            b"transcript",
            b"root-session.jsonl",
            1,
        )
        .unwrap()
    }

    fn source_identity() -> ScopedSourceObjectIdentity {
        ScopedSourceObjectIdentity::from_semantic_context(&semantic_context()).unwrap()
    }

    fn record_with_observed_at(
        generation: u64,
        start: u64,
        end: u64,
        observed_at: i64,
    ) -> SourceRecord {
        SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 11,
                stream_id: 22,
                object_id: 33,
                observed_at,
                source_timestamp_hint: Some(43),
                media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
            },
            generation,
            SourceCursor::append_offset(start),
            SourceCursor::append_offset(end),
            0,
            format!("record-{generation}-{start}-{end}").into_bytes(),
        )
    }

    fn record(generation: u64, start: u64, end: u64) -> SourceRecord {
        record_with_observed_at(generation, start, end, 44)
    }

    fn exact_value<T>(value: T, native_field: &str) -> UsageQualifiedValue<T> {
        QualifiedValue::from_parts(
            Some(value),
            QualifiedValueQuality::Exact,
            UsageValueAuthority::NativeResponse,
            ContractCompleteness::Complete,
            None,
            None,
            UsageValueProvenance {
                native_field: native_field.to_string(),
                normalization_contract_version: 1,
            },
        )
        .unwrap()
    }

    fn usage_fact(batch: &FactBatch, response_key: &str, input_tokens: u64) -> UsageRevisionV2Fact {
        UsageRevisionV2Fact {
            session: batch
                .canonical_entity_key("session", b"native-session")
                .unwrap(),
            actor_run: batch
                .canonical_entity_key("actor-run", b"native-run")
                .unwrap(),
            response_key: response_key.as_bytes().to_vec(),
            response_identity: UsageResponseIdentity::NativeMessageId,
            native_message_id: Some(response_key.to_string()),
            request_id: Some("request-1".to_string()),
            buckets: UsageBucketsV2 {
                input_tokens: exact_value(input_tokens, "message.usage.input_tokens"),
                output_tokens: exact_value(2, "message.usage.output_tokens"),
                cache_creation_input_tokens: exact_value(
                    3,
                    "message.usage.cache_creation_input_tokens",
                ),
                cache_read_input_tokens: exact_value(4, "message.usage.cache_read_input_tokens"),
            },
            model: Some(exact_value("model-1".to_string(), "message.model")),
            effort: None,
            source_time: Some(QualifiedTimestamp {
                value: "2026-08-16T00:00:00Z".to_string(),
                quality: TimestampQuality::NativeExact,
            }),
        }
    }

    fn usage_batch(
        record: &SourceRecord,
        response_key: &str,
        input_tokens: u64,
        forged_revision_key: Option<&[u8]>,
    ) -> FactBatch {
        let mut batch = FactBatch::new_with_semantic_context(8, 4, semantic_context()).unwrap();
        let usage = usage_fact(&batch, response_key, input_tokens);
        let revision_key = usage.semantic_revision_key().unwrap();
        batch
            .push_native_object_scoped_with_revision(
                record,
                response_key.as_bytes(),
                forged_revision_key.unwrap_or(&revision_key),
                Fact::UsageRevisionV2(usage),
            )
            .unwrap();
        batch
    }

    fn decoded_frame(
        lane_ordinal: u64,
        phase: ScopedAppendDeliveryPhase,
        record: &SourceRecord,
        batch: FactBatch,
    ) -> ScopedQueuedObservationFrame {
        ScopedQueuedObservationFrame::Decoded {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal,
            phase,
            item: Box::new(ScopedDecodedAppendItem::Record {
                evidence: Box::new(scoped_record_evidence(record, RawRetentionPolicy::None)),
                disposition: DecodeDisposition::Applied,
                batch,
                quarantined: false,
            }),
        }
    }

    fn only_usage_event(projected: Vec<ScopedProjectedObservation>) -> ScopedUsageV2Event {
        assert_eq!(projected.len(), 1);
        let ScopedProjectedObservation::UsageV2 { event, .. } =
            projected.into_iter().next().unwrap()
        else {
            panic!("expected one usage-v2 event");
        };
        *event
    }

    fn sink(max_usage_v2_entities: usize) -> ScopedObservationProjectionSink {
        ScopedObservationProjectionSink::new(ScopedObservationProjectionLimits {
            max_usage_v2_entities,
        })
        .unwrap()
    }

    fn bytes_hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn root_identity() -> ScopedObservationRootIdentity {
        let context = semantic_context();
        let session_key = CanonicalEntityKey::derive(
            context.adapter_id().as_str(),
            &context.source_instance_key(),
            "session",
            b"native-session",
        )
        .unwrap();
        let session_ref = ExternalEntityRef::new(session_key);
        let identity: QualifiedValue<NativeIdentity> = QualifiedValue::from_parts(
            Some(NativeIdentity {
                native_namespace: "fixture.session".to_string(),
                native_id: "native-session".to_string(),
            }),
            QualifiedValueQuality::NativeClaimed,
            "fixture".to_string(),
            ContractCompleteness::Complete,
            None,
            None,
            Vec::new(),
        )
        .unwrap();
        ScopedRootIdentityRequest::new(
            1,
            b"stable-source-instance".as_slice(),
            b"native-session".as_slice(),
            None,
            Some(session_key),
            Some(session_ref),
        )
        .with_native_session_claim(NativeIdentityClaim::new(session_ref, identity).unwrap())
        .resolve(context.adapter_id(), EXTERNAL_ENTITY_REFERENCE_VERSION)
        .unwrap()
    }

    fn admission_lane_with_decoded_frame(
        frame: ScopedQueuedObservationFrame,
    ) -> ScopedObservationAdmissionLane {
        let ScopedQueuedObservationFrame::Decoded {
            object_token,
            source,
            lane_ordinal,
            phase,
            item,
        } = frame
        else {
            panic!("expected decoded frame")
        };
        let (data_events, retained_native_bytes) = decoded_item_measurement(&item).unwrap();
        let mut lane = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events: data_events.max(1),
            max_retained_native_bytes: retained_native_bytes,
            max_control_items: 1,
            max_coverage_objects: 1,
        })
        .unwrap();
        lane.decoded.push_back(QueuedDecodedFrame {
            object_token,
            source,
            lane_ordinal,
            phase,
            item,
            data_events,
            retained_native_bytes,
        });
        lane.queued_data_events = data_events;
        lane.queued_retained_native_bytes = retained_native_bytes;
        lane.next_lane_ordinal = lane_ordinal + 1;
        lane
    }

    fn delivery_lane(
        max_semantic_events: usize,
        max_source_control_items: usize,
    ) -> ScopedObservationDeliveryLane {
        ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
            max_semantic_events,
            max_retained_native_bytes: 0,
            max_source_control_items,
        })
        .unwrap()
    }

    #[test]
    fn scoped_source_controls_use_coverage_identity_and_stable_event_ids() {
        let source = source_identity();
        let context = semantic_context();
        assert_eq!(source.adapter_id.as_str(), "fixture");
        assert_eq!(source.source_instance_key, context.source_instance_key());
        assert_eq!(
            source.stream_key,
            CoverageStreamKey::derive("fixture", b"transcript").unwrap()
        );
        assert_eq!(
            source.object_key,
            CoverageObjectKey::derive("transcript", b"root-session.jsonl").unwrap()
        );

        let reset = ScopedAppendReset {
            old_generation: 1,
            new_generation: 2,
            reason: AppendTransition::Truncated,
        };
        let first_frame = ScopedQueuedObservationFrame::Reset {
            object_token: 1,
            source: source.clone(),
            lane_ordinal: 1,
            observed_at: 44,
            phase: ScopedAppendDeliveryPhase::Correction,
            reset,
        };
        let replay_frame = ScopedQueuedObservationFrame::Reset {
            object_token: 99,
            source: source.clone(),
            lane_ordinal: 88,
            observed_at: 99,
            phase: ScopedAppendDeliveryPhase::Bootstrap,
            reset,
        };
        let mut first_projection = sink(8);
        let first = first_projection.project(&first_frame).unwrap();
        let mut replay_projection = sink(8);
        let replay = replay_projection.project(&replay_frame).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(replay.len(), 1);
        assert_eq!(first[0].event_id(), replay[0].event_id());
        assert_eq!(first[0].semantic_revision_ref(), None);
        assert_eq!(first[0].phase(), ScopedAppendDeliveryPhase::Correction);
        assert_eq!(first[0].source(), &source);
        assert_eq!(
            bytes_hex(first[0].event_id().as_bytes()),
            "7c7a71b1c39cfbf9561387c0308cf4e817df82574e78b03c2117c55632465161"
        );

        let creation = ScopedProjectedObservation::SourcePresence {
            object_token: 1,
            source: source.clone(),
            lane_ordinal: 1,
            observed_at: 44,
            phase: ScopedAppendDeliveryPhase::Live,
            event_id: source_presence_event_id(
                &source,
                ScopedAppendPresenceChange::Created { generation: 2 },
            ),
            change: ScopedAppendPresenceChange::Created { generation: 2 },
        };
        assert_ne!(first[0].event_id(), creation.event_id());

        let record = record(2, 0, 10);
        let usage_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        let usage = first_projection.project(&usage_frame).unwrap();
        assert_eq!(usage.len(), 1);
        assert!(usage[0].semantic_revision_ref().is_some());
        assert_eq!(usage[0].phase(), ScopedAppendDeliveryPhase::Live);
        assert_eq!(usage[0].source(), &source);
    }

    #[test]
    fn scoped_envelope_mapper_routes_typed_usage_without_exposing_internal_ordinals() {
        let root = root_identity();
        let mapper = ScopedObservationEnvelopeMapper::new(root.clone());
        let record = record(3, 10, 25);
        let frame = decoded_frame(
            7,
            ScopedAppendDeliveryPhase::Live,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        let mut projection = sink(8);
        let projected = projection.project(&frame).unwrap();
        let mut delivery = delivery_lane(1, 1);
        delivery.offer(projected).unwrap();
        let delivered = delivery.pop_next().unwrap();
        let expected_event_id = delivered.event_id;
        let expected_semantic_ref = delivered.semantic_revision_ref;
        let envelope = mapper.map(delivered).unwrap();

        assert_eq!(
            envelope.contract_version,
            SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
        );
        assert_eq!(envelope.observer_sequence, 1);
        assert_eq!(envelope.scope_epoch, SCOPED_INITIAL_SCOPE_EPOCH);
        assert_eq!(envelope.event_id, expected_event_id);
        assert_eq!(envelope.semantic_revision_ref, expected_semantic_ref);
        assert_eq!(envelope.root.session_key, root.session_key);
        assert_eq!(envelope.root.session_ref, root.session_ref);
        assert_eq!(
            envelope
                .root
                .native_session_claim
                .as_ref()
                .unwrap()
                .entity_ref,
            root.session_ref
        );
        assert_eq!(envelope.actor.root_session_key, root.session_key);
        assert_ne!(envelope.actor.run_key, root.root_actor_run_key);
        assert_eq!(envelope.actor.role, ActorRunRole::Child);
        assert_eq!(envelope.actor.parent_run_key, None);
        assert_eq!(
            envelope.actor.native_session_id.as_deref(),
            Some("native-session")
        );
        assert_eq!(
            envelope.actor_attribution,
            ScopedActorAttribution::DerivedExact
        );
        assert_eq!(
            envelope.affiliations.completeness,
            ContractCompleteness::Unknown
        );
        assert!(envelope.affiliations.derived_from_revision_refs.is_empty());
        assert_eq!(envelope.source.instance_key, root.source_instance_key);
        assert_eq!(envelope.source.generation, 3);
        assert_eq!(envelope.source.record_index, Some(0));
        assert_eq!(
            envelope.source.byte_range,
            Some(ScopedObservationByteRange { start: 10, end: 25 })
        );
        assert_eq!(envelope.observed_at, 44);
        assert_eq!(
            envelope
                .native_time
                .as_ref()
                .map(|value| value.value.as_str()),
            Some("2026-08-16T00:00:00Z")
        );
        assert_eq!(
            envelope.evidence.authority,
            ScopedEnvelopeEvidenceAuthority::NativeRecord
        );
        assert_eq!(envelope.evidence.quality, QualifiedValueQuality::Exact);
        assert!(matches!(
            envelope.native_evidence,
            ScopedNativeEvidence::Withheld {
                reason: ScopedNativeEvidenceWithheldReason::ProjectionBoundary,
                ..
            }
        ));
        assert!(matches!(
            envelope.event,
            ScopedObservationEvent::UsageV2 {
                operation: ScopedUsageV2Operation::Upsert,
                ..
            }
        ));
    }

    #[test]
    fn scoped_envelope_mapper_marks_controls_as_fallback_and_retractions_with_control_time() {
        let root = root_identity();
        let mapper = ScopedObservationEnvelopeMapper::new(root.clone());
        let record = record_with_observed_at(1, 0, 10, 44);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        let mut projection = sink(8);
        only_usage_event(projection.project(&frame).unwrap());

        let reset = ScopedAppendReset {
            old_generation: 1,
            new_generation: 2,
            reason: AppendTransition::Truncated,
        };
        let reset_frame = ScopedQueuedObservationFrame::Reset {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal: 2,
            observed_at: 777,
            phase: ScopedAppendDeliveryPhase::Correction,
            reset,
        };
        let projected = projection.project(&reset_frame).unwrap();
        let mut delivery = delivery_lane(1, 1);
        delivery.offer(projected).unwrap();

        let control = mapper.map(delivery.pop_next().unwrap()).unwrap();
        assert_eq!(control.actor.run_key, root.root_actor_run_key);
        assert_eq!(control.actor.role, ActorRunRole::Root);
        assert_eq!(
            control.actor_attribution,
            ScopedActorAttribution::ScopeFallback {
                reason: ScopedActorFallbackReason::SourceLifecycleControl,
            }
        );
        assert_eq!(control.semantic_revision_ref, None);
        assert_eq!(control.source.generation, 2);
        assert_eq!(control.observed_at, 777);
        assert_eq!(
            control.evidence.authority,
            ScopedEnvelopeEvidenceAuthority::EngineControl
        );
        assert!(matches!(
            control.event,
            ScopedObservationEvent::SourceReset { reset: value } if value == reset
        ));

        let retraction = mapper.map(delivery.pop_next().unwrap()).unwrap();
        assert_ne!(retraction.actor.run_key, root.root_actor_run_key);
        assert_eq!(retraction.actor.role, ActorRunRole::Child);
        assert_eq!(
            retraction.actor_attribution,
            ScopedActorAttribution::DerivedExact
        );
        assert!(retraction.semantic_revision_ref.is_some());
        assert_eq!(retraction.observed_at, 777);
        assert_eq!(
            retraction.evidence.authority,
            ScopedEnvelopeEvidenceAuthority::CommonReducer
        );
        assert_eq!(retraction.evidence.quality, QualifiedValueQuality::Derived);
        assert!(matches!(
            retraction.event,
            ScopedObservationEvent::UsageV2 {
                operation: ScopedUsageV2Operation::Retract,
                retraction: Some(ScopedUsageV2RetractionCause::Reset(value)),
                ..
            } if value == reset
        ));
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_envelope_mapper_rejects_cross_root_typed_session() {
        let root = root_identity();
        let mapper = ScopedObservationEnvelopeMapper::new(root);
        let record = record(1, 0, 10);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Live,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        let mut projection = sink(8);
        let projected = projection.project(&frame).unwrap();
        let mut delivery = delivery_lane(1, 1);
        delivery.offer(projected).unwrap();
        let mut delivered = delivery.pop_next().unwrap();
        let ScopedProjectedObservation::UsageV2 { event, .. } = &mut delivered.event else {
            panic!("expected usage-v2 event");
        };
        event.revision.session = CanonicalEntityKey::derive(
            "fixture",
            &semantic_context().source_instance_key(),
            "session",
            b"another-session",
        )
        .unwrap();
        assert_eq!(
            mapper.map(delivered),
            Err(ScopedEnvelopeError::RootSessionMismatch)
        );
    }

    #[test]
    fn scoped_root_native_identity_claim_must_match_derived_session() {
        let root = root_identity();
        let context = semantic_context();
        let other_session = CanonicalEntityKey::derive(
            context.adapter_id().as_str(),
            &context.source_instance_key(),
            "session",
            b"other-session",
        )
        .unwrap();
        let claim = root.native_session_claim.as_ref().unwrap();
        let mismatched = NativeIdentityClaim::new(
            ExternalEntityRef::new(other_session),
            claim.identity.clone(),
        )
        .unwrap();
        let request = ScopedRootIdentityRequest::new(
            1,
            b"stable-source-instance".as_slice(),
            b"native-session".as_slice(),
            None,
            Some(root.session_key),
            Some(root.session_ref),
        )
        .with_native_session_claim(mismatched);
        assert!(matches!(
            request.resolve(context.adapter_id(), EXTERNAL_ENTITY_REFERENCE_VERSION),
            Err(ScopedObservationAccessError::InvalidRootIdentity)
        ));
    }

    #[test]
    fn scoped_delivery_preserves_reset_before_retraction_across_capacity_domains() {
        let mut projection = sink(8);
        let first_record = record(1, 0, 10);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        only_usage_event(projection.project(&frame).unwrap());

        let reset = ScopedAppendReset {
            old_generation: 1,
            new_generation: 2,
            reason: AppendTransition::Truncated,
        };
        let reset_frame = ScopedQueuedObservationFrame::Reset {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal: 2,
            observed_at: 45,
            phase: ScopedAppendDeliveryPhase::Correction,
            reset,
        };
        let projected = projection.project(&reset_frame).unwrap();
        assert_eq!(projected.len(), 2);

        let mut delivery = delivery_lane(1, 1);
        let receipt = delivery.offer(projected).unwrap();
        assert_eq!(receipt.first_offered_sequence, Some(1));
        assert_eq!(receipt.offered_through_sequence, 2);
        assert_eq!(receipt.semantic_events, 1);
        assert_eq!(receipt.source_control_items, 1);
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.queued_source_control_items(), 1);
        assert_eq!(
            delivery.state(),
            ScopedObservationDeliveryState {
                scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
                offered_through_sequence: 2,
                delivered_through_sequence: 0,
                continuity: ScopedObservationContinuity::Bootstrap,
                queued_semantic_events: 1,
                queued_retained_native_bytes: 0,
                queued_source_control_items: 1,
            }
        );

        let first = delivery.pop_next().unwrap();
        assert_eq!(
            first.event_contract_version,
            SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
        );
        assert_eq!(first.observer_sequence, 1);
        assert_eq!(first.scope_epoch, SCOPED_INITIAL_SCOPE_EPOCH);
        assert_eq!(first.event_id, first.event.event_id());
        assert_eq!(first.semantic_revision_ref, None);
        assert_eq!(first.phase, ScopedAppendDeliveryPhase::Correction);
        assert_eq!(&first.source, first.event.source());
        assert!(matches!(
            first.event,
            ScopedProjectedObservation::SourceReset { reset: queued, .. } if queued == reset
        ));
        let second = delivery.pop_next().unwrap();
        assert_eq!(second.observer_sequence, 2);
        assert_eq!(second.scope_epoch, SCOPED_INITIAL_SCOPE_EPOCH);
        assert_eq!(second.event_id, second.event.event_id());
        assert!(second.semantic_revision_ref.is_some());
        assert_eq!(second.phase, ScopedAppendDeliveryPhase::Correction);
        assert!(matches!(
            second.event,
            ScopedProjectedObservation::UsageV2 { event, .. }
                if event.operation == ScopedUsageV2Operation::Retract
        ));
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_delivery_semantic_saturation_does_not_consume_source_control_capacity() {
        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Live,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let mut first_projection = sink(8);
        let first = first_projection.project(&first_frame).unwrap();

        let second_record = record(1, 10, 20);
        let second_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &second_record,
            usage_batch(&second_record, "response-2", 20, None),
        );
        let mut second_projection = sink(8);
        let second = second_projection.project(&second_frame).unwrap();

        let mut delivery = delivery_lane(1, 1);
        delivery.offer(first).unwrap();
        let creation = ScopedAppendPresenceChange::Created { generation: 1 };
        let creation_source = source_identity();
        delivery
            .offer(vec![ScopedProjectedObservation::SourcePresence {
                object_token: OBJECT_TOKEN,
                source: creation_source.clone(),
                lane_ordinal: 2,
                observed_at: 44,
                phase: ScopedAppendDeliveryPhase::Live,
                event_id: source_presence_event_id(&creation_source, creation),
                change: creation,
            }])
            .unwrap();
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.queued_source_control_items(), 1);

        let failure = delivery.offer(second).unwrap_err();
        assert_eq!(failure.error, ScopedDeliveryError::SemanticQueueFull);
        assert_eq!(failure.projected.len(), 1);
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.queued_source_control_items(), 1);

        let first = delivery.pop_next().unwrap();
        assert_eq!(first.observer_sequence, 1);
        assert!(matches!(
            first.event,
            ScopedProjectedObservation::UsageV2 { .. }
        ));
        let second = delivery.pop_next().unwrap();
        assert_eq!(second.observer_sequence, 2);
        assert!(matches!(
            second.event,
            ScopedProjectedObservation::SourcePresence { change, .. } if change == creation
        ));
    }

    #[test]
    fn scoped_bootstrap_barrier_enters_control_lane_while_semantic_lane_is_full() {
        let first_record = record(1, 0, 10);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let mut projection = sink(8);
        let projected = projection.project(&frame).unwrap();
        let mut delivery = delivery_lane(1, 1);
        delivery.offer(projected).unwrap();
        assert_eq!(delivery.queued_semantic_events(), 1);

        let root = root_identity();
        let watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            offered_through_sequence: 1,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: delivery.state(),
        };
        let barrier = delivery
            .offer_bootstrap_barrier(&root, watermark, true, 99)
            .unwrap();
        assert_eq!(barrier.barrier_sequence, 2);
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.queued_source_control_items(), 1);
        assert_eq!(delivery.state(), barrier.queue_state);

        let first = delivery.pop_next().unwrap();
        assert_eq!(first.observer_sequence, 1);
        assert!(matches!(
            first.event,
            ScopedProjectedObservation::UsageV2 { .. }
        ));
        let second = delivery.pop_next().unwrap();
        assert_eq!(second.observer_sequence, 2);
        assert!(matches!(
            second.event,
            ScopedProjectedObservation::ObserverBootstrapComplete {
                barrier: delivered,
                ..
            } if Arc::ptr_eq(&barrier, &delivered)
        ));

        let next_record = record(1, 10, 20);
        let next_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Bootstrap,
            &next_record,
            usage_batch(&next_record, "response-2", 20, None),
        );
        let mut next_projection = sink(8);
        let next = next_projection.project(&next_frame).unwrap();
        let failure = delivery.offer(next).unwrap_err();
        assert_eq!(failure.error, ScopedDeliveryError::BootstrapAlreadyComplete);
        assert_eq!(failure.projected.len(), 1);
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_resync_required_invalidates_backlog_and_delivers_next() {
        let root = root_identity();
        let mapper = ScopedObservationEnvelopeMapper::new(root.clone());
        let bootstrap_record = record(1, 0, 10);
        let bootstrap_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &bootstrap_record,
            usage_batch(&bootstrap_record, "response-1", 10, None),
        );
        let mut projection = sink(8);
        let bootstrap = projection.project(&bootstrap_frame).unwrap();
        let mut delivery = delivery_lane(2, 2);
        delivery.offer(bootstrap).unwrap();
        let watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            offered_through_sequence: 1,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: delivery.state(),
        };
        let barrier = delivery
            .offer_bootstrap_barrier(&root, watermark, true, 50)
            .unwrap();
        assert_eq!(barrier.barrier_sequence, 2);
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 2);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Valid
        );
        assert_eq!(delivery.state().delivered_through_sequence, 2);

        let live_record = record(1, 10, 20);
        let live_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &live_record,
            usage_batch(&live_record, "response-2", 20, None),
        );
        delivery
            .offer(projection.project(&live_frame).unwrap())
            .unwrap();
        let creation = ScopedAppendPresenceChange::Created { generation: 1 };
        let source = source_identity();
        delivery
            .offer(vec![ScopedProjectedObservation::SourcePresence {
                object_token: OBJECT_TOKEN,
                source: source.clone(),
                lane_ordinal: 3,
                observed_at: 60,
                phase: ScopedAppendDeliveryPhase::Live,
                event_id: source_presence_event_id(&source, creation),
                change: creation,
            }])
            .unwrap();
        assert_eq!(delivery.state().offered_through_sequence, 4);
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.queued_source_control_items(), 1);

        let control = delivery
            .require_resync(&root, ScopedResyncReason::WatcherOverflow, 70)
            .unwrap();
        assert_eq!(control.invalid_scope_epoch, 1);
        assert_eq!(control.control_sequence, 5);
        assert_eq!(control.last_contiguous_sequence, 2);
        assert_eq!(control.baseline_snapshot_digest, barrier.snapshot_digest);
        assert_eq!(control.discarded_semantic_events, 1);
        assert_eq!(control.discarded_source_controls, 1);
        assert_eq!(control.discarded_retained_native_bytes, 0);
        assert_eq!(delivery.queued_semantic_events(), 0);
        assert_eq!(delivery.queued_source_control_items(), 1);
        assert_eq!(delivery.state().offered_through_sequence, 5);
        assert_eq!(delivery.state().delivered_through_sequence, 2);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::ResyncRequired
        );

        let repeated = delivery
            .require_resync(&root, ScopedResyncReason::ExplicitConsumerRequest, 999)
            .unwrap();
        assert!(Arc::ptr_eq(&control, &repeated));
        assert_eq!(delivery.queued_source_control_items(), 1);

        let delivered = delivery.pop_next().unwrap();
        assert_eq!(delivered.observer_sequence, 5);
        let envelope = mapper.map(delivered).unwrap();
        assert_eq!(envelope.scope_epoch, 1);
        assert_eq!(envelope.observed_at, 70);
        assert_eq!(envelope.semantic_revision_ref, None);
        assert_eq!(envelope.actor.run_key, root.root_actor_run_key);
        assert_eq!(
            envelope.actor_attribution,
            ScopedActorAttribution::ScopeFallback {
                reason: ScopedActorFallbackReason::ObserverLifecycleControl,
            }
        );
        assert!(matches!(
            envelope.event,
            ScopedObservationEvent::ObserverResyncRequired {
                control: delivered_control,
            } if Arc::ptr_eq(&control, &delivered_control)
        ));
        assert_eq!(delivery.state().delivered_through_sequence, 5);
        assert!(delivery.is_empty());

        let late = ScopedProjectedObservation::SourcePresence {
            object_token: OBJECT_TOKEN,
            source: source.clone(),
            lane_ordinal: 4,
            observed_at: 80,
            phase: ScopedAppendDeliveryPhase::Live,
            event_id: source_presence_event_id(
                &source,
                ScopedAppendPresenceChange::Deleted { generation: 1 },
            ),
            change: ScopedAppendPresenceChange::Deleted { generation: 1 },
        };
        let failure = delivery.offer(vec![late]).unwrap_err();
        assert_eq!(failure.error, ScopedDeliveryError::ResyncRequired);
        assert_eq!(failure.projected.len(), 1);
        assert!(delivery.is_empty());

        let started = delivery.begin_resync(&root, 90).unwrap();
        assert_eq!(started.old_scope_epoch, 1);
        assert_eq!(started.new_scope_epoch, 2);
        assert_eq!(started.control_sequence, 6);
        assert_eq!(started.required_control_sequence, 5);
        assert_eq!(started.baseline_snapshot_digest, barrier.snapshot_digest);
        assert_eq!(started.reason, ScopedResyncReason::WatcherOverflow);
        assert_eq!(started.replacement, ScopedReplacementMode::FullSnapshot);
        let repeated_started = delivery.begin_resync(&root, 999).unwrap();
        assert!(Arc::ptr_eq(&started, &repeated_started));
        assert_eq!(delivery.state().scope_epoch, 2);
        assert_eq!(delivery.state().offered_through_sequence, 6);
        assert_eq!(delivery.state().delivered_through_sequence, 5);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Resyncing
        );
        assert_eq!(delivery.queued_source_control_items(), 1);

        let live_during_resync = ScopedProjectedObservation::SourcePresence {
            object_token: OBJECT_TOKEN,
            source: source.clone(),
            lane_ordinal: 5,
            observed_at: 100,
            phase: ScopedAppendDeliveryPhase::Live,
            event_id: source_presence_event_id(
                &source,
                ScopedAppendPresenceChange::Deleted { generation: 1 },
            ),
            change: ScopedAppendPresenceChange::Deleted { generation: 1 },
        };
        let failure = delivery.offer(vec![live_during_resync]).unwrap_err();
        assert_eq!(failure.error, ScopedDeliveryError::InvalidResyncPhase);
        assert_eq!(failure.projected.len(), 1);

        let correction = ScopedProjectedObservation::SourcePresence {
            object_token: OBJECT_TOKEN,
            source: source.clone(),
            lane_ordinal: 6,
            observed_at: 110,
            phase: ScopedAppendDeliveryPhase::Correction,
            event_id: source_presence_event_id(
                &source,
                ScopedAppendPresenceChange::Deleted { generation: 1 },
            ),
            change: ScopedAppendPresenceChange::Deleted { generation: 1 },
        };
        let receipt = delivery.offer(vec![correction]).unwrap();
        assert_eq!(receipt.first_offered_sequence, Some(7));
        assert_eq!(receipt.offered_through_sequence, 7);

        let delivered_start = delivery.pop_next().unwrap();
        assert_eq!(delivered_start.scope_epoch, 2);
        assert_eq!(delivered_start.observer_sequence, 6);
        assert_eq!(
            delivered_start.event_id,
            resync_started_event_id(&root, &started)
        );
        let start_envelope = mapper.map(delivered_start).unwrap();
        assert_eq!(start_envelope.scope_epoch, 2);
        assert_eq!(start_envelope.observer_sequence, 6);
        assert_eq!(start_envelope.observed_at, 90);
        assert_eq!(start_envelope.semantic_revision_ref, None);
        assert_eq!(start_envelope.actor.run_key, root.root_actor_run_key);
        assert_eq!(
            start_envelope.actor_attribution,
            ScopedActorAttribution::ScopeFallback {
                reason: ScopedActorFallbackReason::ObserverLifecycleControl,
            }
        );
        assert!(matches!(
            start_envelope.event,
            ScopedObservationEvent::ObserverResyncStarted {
                control: delivered_control,
            } if Arc::ptr_eq(&started, &delivered_control)
        ));

        let delivered_correction = delivery.pop_next().unwrap();
        assert_eq!(delivered_correction.scope_epoch, 2);
        assert_eq!(delivered_correction.observer_sequence, 7);
        assert_eq!(
            delivered_correction.phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert!(matches!(
            mapper.map(delivered_correction).unwrap().event,
            ScopedObservationEvent::SourcePresence {
                change: ScopedAppendPresenceChange::Deleted { generation: 1 },
            }
        ));
        assert_eq!(delivery.state().delivered_through_sequence, 7);
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_resync_required_cannot_be_fabricated_before_bootstrap() {
        let root = root_identity();
        let mut delivery = delivery_lane(1, 1);
        assert_eq!(
            delivery.require_resync(&root, ScopedResyncReason::WatcherOverflow, 10),
            Err(ScopedContinuityError::BootstrapIncomplete)
        );
        assert!(delivery.resync_required().is_none());
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Bootstrap
        );
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_resync_start_requires_delivered_invalidation_control() {
        let root = root_identity();
        let mut delivery = delivery_lane(1, 1);
        assert_eq!(
            delivery.begin_resync(&root, 10),
            Err(ScopedContinuityError::ResyncNotRequired)
        );

        let watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            offered_through_sequence: 0,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: delivery.state(),
        };
        delivery
            .offer_bootstrap_barrier(&root, watermark, false, 20)
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);
        let required = delivery
            .require_resync(&root, ScopedResyncReason::TransportContinuityLoss, 30)
            .unwrap();
        assert_eq!(required.control_sequence, 2);
        assert_eq!(
            delivery.begin_resync(&root, 40),
            Err(ScopedContinuityError::ResyncRequiredNotDelivered)
        );
        assert_eq!(delivery.state().scope_epoch, 1);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::ResyncRequired
        );
        assert_eq!(delivery.queued_source_control_items(), 1);
        assert!(delivery.resync_started().is_none());
    }

    #[test]
    fn scoped_replacement_stage_isolates_active_state_and_publishes_latest_snapshot() {
        let root = root_identity();
        let mut active = sink(8);
        let active_record = record(1, 0, 10);
        let active_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &active_record,
            usage_batch(&active_record, "active-response", 10, None),
        );
        only_usage_event(active.project(&active_frame).unwrap());

        let mut delivery = delivery_lane(1, 1);
        let watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            offered_through_sequence: 0,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: delivery.state(),
        };
        delivery
            .offer_bootstrap_barrier(&root, watermark, true, 20)
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);
        delivery
            .require_resync(&root, ScopedResyncReason::WatcherOverflow, 30)
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 2);
        delivery.begin_resync(&root, 40).unwrap();

        let first_replay_record = record(1, 10, 20);
        let first_replay_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_replay_record,
            usage_batch(&first_replay_record, "replacement-response", 20, None),
        );
        let mut first_replay = admission_lane_with_decoded_frame(first_replay_frame);
        assert_eq!(
            first_replay.offer_next(&mut active, &mut delivery),
            Err(ScopedProjectionDeliveryError::Projection(
                ScopedProjectionError::InvalidLifecycle
            ))
        );
        assert!(!first_replay.is_empty());
        assert_eq!(active.usage_v2_entity_count(), 1);

        let mut stage =
            ScopedObservationReplacementStage::new(root.clone(), 2, active.limits).unwrap();
        assert!(stage.reduce_next(&mut first_replay).unwrap());
        assert!(!stage.reduce_next(&mut first_replay).unwrap());
        assert!(first_replay.is_empty());
        assert_eq!(stage.usage_v2_entity_count(), 1);
        assert_eq!(active.usage_v2_entity_count(), 1);

        let latest_replay_record = record(1, 20, 30);
        let latest_replay_frame = decoded_frame(
            3,
            ScopedAppendDeliveryPhase::Live,
            &latest_replay_record,
            usage_batch(&latest_replay_record, "replacement-response", 30, None),
        );
        let mut latest_replay = admission_lane_with_decoded_frame(latest_replay_frame);
        assert!(stage.reduce_next(&mut latest_replay).unwrap());
        assert!(latest_replay.is_empty());

        let snapshot = stage.prepare_snapshot(&latest_replay, &delivery).unwrap();
        assert_eq!(snapshot.phase, ScopedAppendDeliveryPhase::Correction);
        assert_eq!(snapshot.entity_count, 1);
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(
            snapshot.events[0].revision.response_key,
            b"replacement-response"
        );
        assert_eq!(
            snapshot.events[0].revision.buckets.input_tokens.value,
            Some(30)
        );
        let replacement_semantic_digest = snapshot.semantic_digest;
        assert_eq!(
            stage.reduce_next(&mut latest_replay),
            Err(ScopedReplacementStageError::SnapshotAlreadyPrepared)
        );

        let active_snapshot = active
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();
        assert_eq!(active_snapshot.entity_count, 1);
        assert_eq!(
            active_snapshot.events[0].revision.response_key,
            b"active-response"
        );

        assert!(!stage.snapshot_fully_offered());
        let receipt = stage.offer_snapshot_next(&mut delivery).unwrap().unwrap();
        assert_eq!(receipt.first_offered_sequence, Some(4));
        assert_eq!(receipt.offered_through_sequence, 4);
        assert_eq!(stage.offer_snapshot_next(&mut delivery), Ok(None));
        assert!(stage.snapshot_fully_offered());

        let start = delivery.pop_next().unwrap();
        assert_eq!(start.observer_sequence, 3);
        assert!(matches!(
            start.event,
            ScopedProjectedObservation::ObserverResyncStarted { .. }
        ));
        let replacement = delivery.pop_next().unwrap();
        assert_eq!(replacement.scope_epoch, 2);
        assert_eq!(replacement.observer_sequence, 4);
        assert!(matches!(
            replacement.event,
            ScopedProjectedObservation::UsageV2 { event, .. }
                if event.phase == ScopedAppendDeliveryPhase::Correction
                    && event.revision.response_key == b"replacement-response"
                    && event.revision.buckets.input_tokens.value == Some(30)
        ));
        assert!(delivery.is_empty());

        let family_manifest = vec![ScopedReplacementFamilyManifest {
            fact_family: "runtime.usage-v2".to_string(),
            contract_version: RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION,
            replacement_representation:
                ScopedReplacementRepresentation::UsageLatestContributionPerResponse,
            completeness: CoverageSetCompleteness::Complete,
            entity_or_event_count: 1,
            semantic_digest: replacement_semantic_digest,
        }];
        let replacement_snapshot_digest =
            replacement_snapshot_digest(&root, true, &family_manifest, &[], &[]).unwrap();
        let completion_watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: 2,
            offered_through_sequence: 4,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: delivery.state(),
        };
        assert_eq!(
            delivery.offer_resync_barrier(
                &root,
                completion_watermark.clone(),
                family_manifest.clone(),
                ScopedReplacementSnapshotDigest([0; 32]),
                true,
                120,
            ),
            Err(ScopedReplacementStageError::InvalidManifest)
        );
        assert_eq!(delivery.state().offered_through_sequence, 4);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Resyncing
        );
        let barrier = delivery
            .offer_resync_barrier(
                &root,
                completion_watermark,
                family_manifest,
                replacement_snapshot_digest,
                true,
                120,
            )
            .unwrap();
        assert_eq!(barrier.barrier_sequence, 5);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Valid
        );
        stage.validate_activation(&active).unwrap();
        stage.activate(&mut active, Arc::clone(&barrier));

        let activated = active
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();
        assert_eq!(activated.entity_count, 1);
        assert_eq!(
            activated.events[0].revision.response_key,
            b"replacement-response"
        );
        let retired = stage
            .projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();
        assert_eq!(retired.entity_count, 1);
        assert_eq!(retired.events[0].revision.response_key, b"active-response");
        assert_eq!(
            stage.projection.lifecycle,
            ScopedProjectionLifecycle::Retired
        );

        let completed = delivery.pop_next().unwrap();
        assert_eq!(completed.scope_epoch, 2);
        assert_eq!(completed.observer_sequence, 5);
        let completed_envelope = ScopedObservationEnvelopeMapper::new(root.clone())
            .map(completed)
            .unwrap();
        assert!(matches!(
            completed_envelope.event,
            ScopedObservationEvent::ObserverResyncComplete {
                barrier: delivered,
            } if Arc::ptr_eq(&barrier, &delivered)
        ));
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_delivery_rejects_oversized_batch_without_partial_offer() {
        let mut projection = sink(8);
        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let mut projected = projection.project(&first_frame).unwrap();
        let second_record = record(1, 10, 20);
        let second_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Bootstrap,
            &second_record,
            usage_batch(&second_record, "response-2", 20, None),
        );
        projected.extend(projection.project(&second_frame).unwrap());

        let mut delivery = delivery_lane(1, 1);
        let failure = delivery.offer(projected).unwrap_err();
        assert_eq!(failure.error, ScopedDeliveryError::SemanticBatchTooLarge);
        assert_eq!(failure.projected.len(), 2);
        assert!(delivery.is_empty());

        let empty = delivery.offer(Vec::new()).unwrap();
        assert_eq!(empty.first_offered_sequence, None);
        assert_eq!(empty.offered_through_sequence, 0);
        assert_eq!(empty.semantic_events, 0);
        assert_eq!(empty.source_control_items, 0);
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_delivery_rejects_invalid_limits_and_sequence_exhaustion() {
        assert!(matches!(
            ScopedObservationDeliveryLane::new(ScopedObservationDeliveryLimits {
                max_semantic_events: 0,
                max_retained_native_bytes: 0,
                max_source_control_items: 1,
            }),
            Err(ScopedDeliveryError::InvalidLimits)
        ));

        let record = record(1, 0, 10);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        let mut projection = sink(8);
        let projected = projection.project(&frame).unwrap();
        let mut delivery = delivery_lane(1, 1);
        delivery.next_observer_sequence = u64::MAX;
        let failure = delivery.offer(projected).unwrap_err();
        assert_eq!(
            failure.error,
            ScopedDeliveryError::ObserverSequenceExhausted
        );
        assert_eq!(failure.projected.len(), 1);
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_offer_transaction_retries_without_projection_or_sequence_drift() {
        let mut projection = sink(8);
        let mut delivery = delivery_lane(1, 1);

        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let mut first_lane = admission_lane_with_decoded_frame(first_frame);
        let first_receipt = first_lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(first_receipt.first_offered_sequence, Some(1));
        assert!(first_lane.is_empty());
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.state().offered_through_sequence, 1);

        let second_record = record(1, 10, 20);
        let second_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &second_record,
            usage_batch(&second_record, "response-2", 20, None),
        );
        let mut second_lane = admission_lane_with_decoded_frame(second_frame);
        let queued_events = second_lane.queued_data_events();
        let queued_bytes = second_lane.queued_retained_native_bytes();

        assert_eq!(
            second_lane.offer_next(&mut projection, &mut delivery),
            Err(ScopedProjectionDeliveryError::Delivery(
                ScopedDeliveryError::SemanticQueueFull
            ))
        );
        assert_eq!(second_lane.queued_data_events(), queued_events);
        assert_eq!(second_lane.queued_retained_native_bytes(), queued_bytes);
        assert!(!second_lane.is_empty());
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.state().offered_through_sequence, 1);

        let first = delivery.pop_next().unwrap();
        assert_eq!(first.observer_sequence, 1);
        let retry_receipt = second_lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(retry_receipt.first_offered_sequence, Some(2));
        assert_eq!(retry_receipt.offered_through_sequence, 2);
        assert_eq!(delivery.state().offered_through_sequence, 2);
        assert_eq!(projection.usage_v2_entity_count(), 2);
        assert!(second_lane.is_empty());
        assert_eq!(second_lane.queued_data_events(), 0);
        assert_eq!(second_lane.queued_retained_native_bytes(), 0);

        let second = delivery.pop_next().unwrap();
        assert_eq!(second.observer_sequence, 2);
        assert!(matches!(
            second.event,
            ScopedProjectedObservation::UsageV2 { event, .. }
                if event.revision.response_key == b"response-2"
        ));
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_offer_transaction_retires_exact_repeat_while_delivery_is_full() {
        let mut projection = sink(8);
        let mut delivery = delivery_lane(1, 1);

        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let mut first_lane = admission_lane_with_decoded_frame(first_frame);
        first_lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(delivery.queued_semantic_events(), 1);

        let repeat_record = record(1, 10, 20);
        let repeat_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &repeat_record,
            usage_batch(&repeat_record, "response-1", 10, None),
        );
        let mut repeat_lane = admission_lane_with_decoded_frame(repeat_frame);
        let receipt = repeat_lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.first_offered_sequence, None);
        assert_eq!(receipt.offered_through_sequence, 1);
        assert_eq!(receipt.semantic_events, 0);
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert!(repeat_lane.is_empty());
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.state().offered_through_sequence, 1);

        let first = delivery.pop_next().unwrap();
        assert_eq!(first.observer_sequence, 1);
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_offer_transaction_keeps_reset_and_reducer_state_on_capacity_failure() {
        let mut projection = sink(8);
        let mut delivery = delivery_lane(1, 1);
        let record = record(1, 0, 10);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        let mut initial_lane = admission_lane_with_decoded_frame(frame);
        initial_lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        let before = projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();

        let reset = ScopedAppendReset {
            old_generation: 1,
            new_generation: 2,
            reason: AppendTransition::Truncated,
        };
        let mut reset_lane = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events: 1,
            max_retained_native_bytes: 0,
            max_control_items: 1,
            max_coverage_objects: 1,
        })
        .unwrap();
        reset_lane.controls.push_back(QueuedControlFrame {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal: 2,
            observed_at: 45,
            phase: ScopedAppendDeliveryPhase::Correction,
            kind: QueuedControlKind::Reset(reset),
        });

        assert_eq!(
            reset_lane.offer_next(&mut projection, &mut delivery),
            Err(ScopedProjectionDeliveryError::Delivery(
                ScopedDeliveryError::SemanticQueueFull
            ))
        );
        assert_eq!(reset_lane.queued_control_items(), 1);
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert_eq!(
            projection
                .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
                .unwrap(),
            before
        );
        assert_eq!(delivery.queued_semantic_events(), 1);
        assert_eq!(delivery.queued_source_control_items(), 0);

        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);
        let receipt = reset_lane
            .offer_next(&mut projection, &mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.first_offered_sequence, Some(2));
        assert_eq!(receipt.offered_through_sequence, 3);
        assert_eq!(receipt.semantic_events, 1);
        assert_eq!(receipt.source_control_items, 1);
        assert!(reset_lane.is_empty());
        assert_eq!(projection.usage_v2_entity_count(), 0);

        let reset_offered = delivery.pop_next().unwrap();
        assert_eq!(reset_offered.observer_sequence, 2);
        assert!(matches!(
            reset_offered.event,
            ScopedProjectedObservation::SourceReset { reset: queued, .. } if queued == reset
        ));
        let retraction = delivery.pop_next().unwrap();
        assert_eq!(retraction.observer_sequence, 3);
        assert!(matches!(
            retraction.event,
            ScopedProjectedObservation::UsageV2 { event, .. }
                if event.operation == ScopedUsageV2Operation::Retract
                    && event.retraction == Some(ScopedUsageV2RetractionCause::Reset(reset))
        ));
        assert!(delivery.is_empty());
    }

    #[test]
    fn scoped_projection_consumption_releases_only_accepted_frame_accounting() {
        let record = record(1, 0, 10);
        let frame = decoded_frame(
            7,
            ScopedAppendDeliveryPhase::Bootstrap,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        let mut lane = admission_lane_with_decoded_frame(frame);
        let queued_events = lane.queued_data_events();
        let queued_bytes = lane.queued_retained_native_bytes();
        let mut projection = sink(8);

        let projected = lane.project_next(&mut projection).unwrap().unwrap();
        let event = only_usage_event(projected);
        assert_eq!(event.phase, ScopedAppendDeliveryPhase::Bootstrap);
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert_eq!(queued_events, 1);
        assert_eq!(queued_bytes, 0);
        assert_eq!(lane.queued_data_events(), 0);
        assert_eq!(lane.queued_retained_native_bytes(), 0);
        assert!(lane.is_empty());
        assert_eq!(lane.project_next(&mut projection), Ok(None));
    }

    #[test]
    fn scoped_projection_failure_keeps_decoded_frame_and_accounting_for_retry() {
        let mut projection = sink(8);
        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let first = only_usage_event(projection.project(&first_frame).unwrap());

        let forged_record = record(1, 10, 20);
        let forged_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &forged_record,
            usage_batch(&forged_record, "response-1", 20, Some(b"forged-revision")),
        );
        let mut lane = admission_lane_with_decoded_frame(forged_frame);
        let queued_events = lane.queued_data_events();
        let queued_bytes = lane.queued_retained_native_bytes();

        for _ in 0..2 {
            assert_eq!(
                lane.project_next(&mut projection),
                Err(ScopedProjectionError::InvalidSemanticRevision)
            );
            assert_eq!(lane.queued_data_events(), queued_events);
            assert_eq!(lane.queued_retained_native_bytes(), queued_bytes);
            assert!(!lane.is_empty());
            assert_eq!(projection.usage_v2_entity_count(), 1);
            assert_eq!(
                projection.usage_v2_revision(&first.fact_id),
                Some(first.semantic_revision_ref)
            );
        }

        assert!(matches!(
            lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Decoded {
                lane_ordinal: 2,
                ..
            })
        ));
    }

    #[test]
    fn scoped_projection_failure_keeps_control_queued_for_retry() {
        let mut projection = sink(8);
        let record = record(1, 0, 10);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        only_usage_event(projection.project(&frame).unwrap());

        let reset = ScopedAppendReset {
            old_generation: 2,
            new_generation: 3,
            reason: AppendTransition::Truncated,
        };
        let mut lane = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
            max_data_events: 1,
            max_retained_native_bytes: 0,
            max_control_items: 1,
            max_coverage_objects: 1,
        })
        .unwrap();
        lane.controls.push_back(QueuedControlFrame {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal: 2,
            observed_at: 45,
            phase: ScopedAppendDeliveryPhase::Correction,
            kind: QueuedControlKind::Reset(reset),
        });

        assert_eq!(
            lane.project_next(&mut projection),
            Err(ScopedProjectionError::InvalidResetState)
        );
        assert_eq!(lane.queued_control_items(), 1);
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert!(matches!(
            lane.pop_next(),
            Some(ScopedQueuedObservationFrame::Reset {
                lane_ordinal: 2,
                reset: queued,
                ..
            }) if queued == reset
        ));
    }

    #[test]
    fn scoped_usage_replacement_snapshot_is_phase_independent_and_rejects_live() {
        let mut projection = sink(8);
        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let first = only_usage_event(projection.project(&first_frame).unwrap());

        let bootstrap = projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Bootstrap)
            .unwrap();
        let correction = projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();

        assert_eq!(
            bootstrap.fact_family_contract_version,
            RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION
        );
        assert_eq!(
            bootstrap.replacement_digest_contract_version,
            SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION
        );
        assert_eq!(bootstrap.phase, ScopedAppendDeliveryPhase::Bootstrap);
        assert_eq!(correction.phase, ScopedAppendDeliveryPhase::Correction);
        assert_eq!(bootstrap.entity_count, 1);
        assert_eq!(bootstrap.semantic_digest, correction.semantic_digest);
        assert_eq!(bootstrap.events.len(), 1);
        assert_eq!(correction.events.len(), 1);
        assert_eq!(bootstrap.events[0].event_id, first.event_id);
        assert_eq!(correction.events[0].event_id, first.event_id);
        assert_eq!(
            bytes_hex(bootstrap.semantic_digest.as_bytes()),
            "4aa769b30dbd71d8f26fcea5a49c76df6d6092da8acb6b1006303ea22e4bf091"
        );
        assert_eq!(
            bytes_hex(first.event_id.as_bytes()),
            "e9160c833cc2d6509935c5c999c6528f22d3aebbdd7418ceebdfeb1284ca4e0e"
        );
        assert_eq!(
            bootstrap.events[0].semantic_revision_ref,
            correction.events[0].semantic_revision_ref
        );
        assert_eq!(
            bootstrap.events[0].phase,
            ScopedAppendDeliveryPhase::Bootstrap
        );
        assert_eq!(
            correction.events[0].phase,
            ScopedAppendDeliveryPhase::Correction
        );
        assert_eq!(
            projection.usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Live),
            Err(ScopedProjectionError::InvalidReplacementPhase)
        );
    }

    #[test]
    fn scoped_usage_replacement_snapshot_has_stable_order_and_digest() {
        let first_record = record(1, 0, 10);
        let second_record = record(1, 10, 20);

        let mut forward = sink(8);
        for (ordinal, source, response, tokens) in [
            (1, &first_record, "response-1", 10),
            (2, &second_record, "response-2", 20),
        ] {
            let frame = decoded_frame(
                ordinal,
                ScopedAppendDeliveryPhase::Bootstrap,
                source,
                usage_batch(source, response, tokens, None),
            );
            only_usage_event(forward.project(&frame).unwrap());
        }

        let mut reverse = sink(8);
        for (ordinal, source, response, tokens) in [
            (1, &second_record, "response-2", 20),
            (2, &first_record, "response-1", 10),
        ] {
            let frame = decoded_frame(
                ordinal,
                ScopedAppendDeliveryPhase::Bootstrap,
                source,
                usage_batch(source, response, tokens, None),
            );
            only_usage_event(reverse.project(&frame).unwrap());
        }

        let forward = forward
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Bootstrap)
            .unwrap();
        let reverse = reverse
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Bootstrap)
            .unwrap();
        assert_eq!(forward.entity_count, 2);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn scoped_usage_replacement_digest_excludes_observation_time() {
        let early_record = record_with_observed_at(1, 0, 10, 44);
        let late_record = record_with_observed_at(1, 0, 10, 99);

        let mut early = sink(8);
        let early_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &early_record,
            usage_batch(&early_record, "response-1", 10, None),
        );
        only_usage_event(early.project(&early_frame).unwrap());

        let mut late = sink(8);
        let late_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &late_record,
            usage_batch(&late_record, "response-1", 10, None),
        );
        only_usage_event(late.project(&late_frame).unwrap());

        let early = early
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Bootstrap)
            .unwrap();
        let late = late
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Bootstrap)
            .unwrap();
        assert_eq!(early.semantic_digest, late.semantic_digest);
        assert_eq!(early.events[0].event_id, late.events[0].event_id);
        assert_ne!(
            early.events[0].source.provenance.observed_at,
            late.events[0].source.provenance.observed_at
        );
    }

    #[test]
    fn scoped_usage_replacement_snapshot_removes_deleted_entities() {
        let mut projection = sink(8);
        let record = record(1, 0, 10);
        let frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &record,
            usage_batch(&record, "response-1", 10, None),
        );
        only_usage_event(projection.project(&frame).unwrap());
        let populated = projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();

        let deletion = ScopedQueuedObservationFrame::Presence {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal: 2,
            observed_at: 45,
            phase: ScopedAppendDeliveryPhase::Correction,
            change: ScopedAppendPresenceChange::Deleted { generation: 1 },
        };
        let removed = projection.project(&deletion).unwrap();
        assert_eq!(removed.len(), 2);

        let replacement = projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();
        let empty = sink(8)
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();
        assert_eq!(replacement, empty);
        assert_eq!(replacement.entity_count, 0);
        assert!(replacement.events.is_empty());
        assert_ne!(populated.semantic_digest, replacement.semantic_digest);
        assert_eq!(
            bytes_hex(replacement.semantic_digest.as_bytes()),
            "ea7d7a39f13d8d04ca52cef36c83e88a9e52f69eb0d7680477a08800a13408c2"
        );
    }

    #[test]
    fn scoped_usage_projection_suppresses_exact_current_repeat_but_preserves_a_b_a() {
        let mut projection = sink(8);

        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let first = only_usage_event(projection.project(&first_frame).unwrap());
        assert_eq!(first.operation, ScopedUsageV2Operation::Upsert);
        assert_eq!(first.phase, ScopedAppendDeliveryPhase::Bootstrap);

        let repeat_record = record(1, 10, 20);
        let repeat_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &repeat_record,
            usage_batch(&repeat_record, "response-1", 10, None),
        );
        assert!(projection.project(&repeat_frame).unwrap().is_empty());

        let second_record = record(1, 20, 30);
        let second_frame = decoded_frame(
            3,
            ScopedAppendDeliveryPhase::Live,
            &second_record,
            usage_batch(&second_record, "response-1", 20, None),
        );
        let second = only_usage_event(projection.project(&second_frame).unwrap());
        assert_ne!(second.semantic_revision_ref, first.semantic_revision_ref);

        let reverted_record = record(1, 30, 40);
        let reverted_frame = decoded_frame(
            4,
            ScopedAppendDeliveryPhase::Live,
            &reverted_record,
            usage_batch(&reverted_record, "response-1", 10, None),
        );
        let reverted = only_usage_event(projection.project(&reverted_frame).unwrap());
        assert_eq!(reverted.semantic_revision_ref, first.semantic_revision_ref);
        assert_ne!(reverted.event_id, first.event_id);
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert_eq!(
            projection.usage_v2_revision(&first.fact_id),
            Some(first.semantic_revision_ref)
        );

        let mut replay_projection = sink(8);
        let replay_frame = decoded_frame(
            9,
            ScopedAppendDeliveryPhase::Correction,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let replay = only_usage_event(replay_projection.project(&replay_frame).unwrap());
        assert_eq!(replay.event_id, first.event_id);
    }

    #[test]
    fn scoped_usage_projection_orders_reset_before_retractions_and_new_generation() {
        let mut projection = sink(8);
        let old_record = record(1, 0, 10);
        let old_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &old_record,
            usage_batch(&old_record, "response-1", 10, None),
        );
        let old = only_usage_event(projection.project(&old_frame).unwrap());

        let reset = ScopedAppendReset {
            old_generation: 1,
            new_generation: 2,
            reason: AppendTransition::Truncated,
        };
        let reset_frame = ScopedQueuedObservationFrame::Reset {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal: 2,
            observed_at: 45,
            phase: ScopedAppendDeliveryPhase::Correction,
            reset,
        };
        let correction = projection.project(&reset_frame).unwrap();
        assert_eq!(correction.len(), 2);
        assert!(matches!(
            correction[0],
            ScopedProjectedObservation::SourceReset { reset: value, .. } if value == reset
        ));
        let ScopedProjectedObservation::UsageV2 {
            event: retracted, ..
        } = &correction[1]
        else {
            panic!("expected reset-owned usage retraction");
        };
        assert_eq!(retracted.operation, ScopedUsageV2Operation::Retract);
        assert_eq!(retracted.semantic_revision_ref, old.semantic_revision_ref);
        assert_eq!(retracted.fact_id, old.fact_id);
        assert_eq!(
            retracted.retraction,
            Some(ScopedUsageV2RetractionCause::Reset(reset))
        );
        assert_eq!(projection.usage_v2_entity_count(), 0);

        let new_record = record(2, 0, 12);
        let new_frame = decoded_frame(
            3,
            ScopedAppendDeliveryPhase::Correction,
            &new_record,
            usage_batch(&new_record, "response-1", 7, None),
        );
        let new = only_usage_event(projection.project(&new_frame).unwrap());
        assert_eq!(new.operation, ScopedUsageV2Operation::Upsert);
        assert_eq!(new.phase, ScopedAppendDeliveryPhase::Correction);
        assert_ne!(new.fact_id, old.fact_id);
        assert_eq!(projection.usage_v2_entity_count(), 1);
    }

    #[test]
    fn scoped_usage_projection_deletion_retracts_state_before_later_creation() {
        let mut projection = sink(8);
        let old_record = record(1, 0, 10);
        let old_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &old_record,
            usage_batch(&old_record, "response-1", 10, None),
        );
        let old = only_usage_event(projection.project(&old_frame).unwrap());

        let deletion = ScopedAppendPresenceChange::Deleted { generation: 1 };
        let deletion_frame = ScopedQueuedObservationFrame::Presence {
            object_token: OBJECT_TOKEN,
            source: source_identity(),
            lane_ordinal: 2,
            observed_at: 45,
            phase: ScopedAppendDeliveryPhase::Live,
            change: deletion,
        };
        let removed = projection.project(&deletion_frame).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(matches!(
            removed[0],
            ScopedProjectedObservation::SourcePresence { change, .. } if change == deletion
        ));
        let ScopedProjectedObservation::UsageV2 {
            event: retracted, ..
        } = &removed[1]
        else {
            panic!("expected deletion-owned usage retraction");
        };
        assert_eq!(retracted.operation, ScopedUsageV2Operation::Retract);
        assert_eq!(retracted.semantic_revision_ref, old.semantic_revision_ref);
        assert_eq!(
            retracted.retraction,
            Some(ScopedUsageV2RetractionCause::SourceDeleted { generation: 1 })
        );
        assert_eq!(projection.usage_v2_entity_count(), 0);

        let creation = ScopedAppendPresenceChange::Created { generation: 2 };
        let creation_source = source_identity();
        let creation_frame = ScopedQueuedObservationFrame::Presence {
            object_token: OBJECT_TOKEN,
            source: creation_source.clone(),
            lane_ordinal: 3,
            observed_at: 46,
            phase: ScopedAppendDeliveryPhase::Live,
            change: creation,
        };
        assert_eq!(
            projection.project(&creation_frame).unwrap(),
            vec![ScopedProjectedObservation::SourcePresence {
                object_token: OBJECT_TOKEN,
                source: creation_source.clone(),
                lane_ordinal: 3,
                observed_at: 46,
                phase: ScopedAppendDeliveryPhase::Live,
                event_id: source_presence_event_id(&creation_source, creation),
                change: creation,
            }]
        );
    }

    #[test]
    fn scoped_usage_projection_rejects_forged_revision_atomically() {
        let mut projection = sink(8);
        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let first = only_usage_event(projection.project(&first_frame).unwrap());

        let forged_record = record(1, 10, 20);
        let forged_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &forged_record,
            usage_batch(&forged_record, "response-1", 20, Some(b"forged-revision")),
        );
        assert_eq!(
            projection.project(&forged_frame),
            Err(ScopedProjectionError::InvalidSemanticRevision)
        );
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert_eq!(
            projection.usage_v2_revision(&first.fact_id),
            Some(first.semantic_revision_ref)
        );
    }

    #[test]
    fn scoped_usage_projection_enforces_entity_capacity_atomically() {
        let mut projection = sink(1);
        let first_record = record(1, 0, 10);
        let first_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Bootstrap,
            &first_record,
            usage_batch(&first_record, "response-1", 10, None),
        );
        let first = only_usage_event(projection.project(&first_frame).unwrap());

        let second_record = record(1, 10, 20);
        let second_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Live,
            &second_record,
            usage_batch(&second_record, "response-2", 20, None),
        );
        assert_eq!(
            projection.project(&second_frame),
            Err(ScopedProjectionError::UsageV2CapacityFull)
        );
        assert_eq!(projection.usage_v2_entity_count(), 1);
        assert_eq!(
            projection.usage_v2_revision(&first.fact_id),
            Some(first.semantic_revision_ref)
        );
    }
}
