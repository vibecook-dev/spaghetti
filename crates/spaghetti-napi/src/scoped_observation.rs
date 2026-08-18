//! Database-free RFC 012 scoped-observation composition root.
//!
//! This module owns the seam between the strict adapter/support registry and
//! common source access. It deliberately exposes no N-API surface yet: native
//! artifact probing and the complete RFC 012D request contract must remain a
//! trusted Rust-host concern until their portable contracts are frozen.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::adapter::{
    ActorRunRole, AdapterError, AdapterErrorClass, AdapterId, AdapterObjectContext,
    AdapterRegistry, AgentAdapter, CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey,
    CompatibilityDecision, ContractCompleteness, CoverageAbsence, CoverageAbsenceKind,
    CoverageDeclarationDigest, CoverageDomain, CoverageError, CoverageObjectKey, CoveragePosition,
    CoveragePositionKind, CoverageProvenance, CoverageScope, CoverageSetCompleteness,
    CoverageStatus, CoverageStreamKey, DecodeDisposition, DecoderId, ExternalEntityRef, Fact,
    FactBatch, FactEnvelope, FactProvenance, FactRevisionId, FactSemanticContext,
    FactSemanticRevision, NativeArtifactProbe, NativeIdentityClaim, QualifiedTimestamp,
    QualifiedValueQuality, RawRetentionPolicy, ScopeRelationPrimitive, SemanticRevisionRef,
    SourceAccess, SourceCoveragePoint, SourceCoverageSet, SourceObjectList,
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
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
    ObservationContractSelection, ObservationNegotiationError,
};
use crate::source::{
    confined_relative_path_key, read_stable_file_confined, validate_relation_id, AccessBudgetError,
    AccessObjectToken, AccessOperation, AccessOutcome, AccessPhase, AppendCheckpoint,
    AppendDelimitedFile, AppendItem, AppendRead, AppendTransition, AuthorizedScopeAccessPlan,
    DirtyHint, DirtyReason, DirtyScope, DriverQuarantine, HintEnqueue, RecordHash, RecordOrigin,
    Revision, ScopeAccessReport, ScopeAccessRequest, ScopeIdentityInput, SourceCursor,
    SourceDriverError, SourceMediaType, SourceRecord, SourceRecordState, StableRead, StartupAction,
    StartupPhase, WatchBeforeScan, MAX_IDENTITY_VALUE_BYTES,
};

/// One exact host-approved object locator. The locator is installed during
/// attachment and cannot be replaced by a decoder or by an access call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedKnownObjectGrant {
    pub relation_id: String,
    /// Exactly one known-object grant is the attachment's semantic root. Barrier
    /// root presence is derived from that object's admitted state.
    pub scope_root: bool,
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
    pub observation_contract_request: ObservationContractRequest,
    pub observation_contract_offer: ObservationContractOffer,
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

pub const SCOPED_SOURCE_OBJECT_ERROR_CONTRACT_VERSION: u32 = 1;

/// Stable machine classification for one relation-local observation failure.
/// Diagnostic strings, native paths, identity inputs, and payload bytes never
/// cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedSourceObjectFailureCode {
    SourceRetryTransient,
    SourceUnstable,
    SourceDatabase,
    SourceIo,
    SourceInvalidConfiguration,
    SourceInvalidCursor,
    SourcePathEscape,
    SourceLimitExceeded,
    DecodeRetryTransient,
    DecodeRecordPermanent,
    DecodeStreamFatal,
}

impl ScopedSourceObjectFailureCode {
    fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::SourceRetryTransient
                | Self::SourceUnstable
                | Self::SourceDatabase
                | Self::SourceIo
                | Self::DecodeRetryTransient
        )
    }

    fn coverage_code(self) -> &'static str {
        match self {
            Self::SourceRetryTransient => "source_retry_transient",
            Self::SourceUnstable => "source_unstable",
            Self::SourceDatabase => "source_database",
            Self::SourceIo => "source_io",
            Self::SourceInvalidConfiguration => "source_invalid_configuration",
            Self::SourceInvalidCursor => "source_invalid_cursor",
            Self::SourcePathEscape => "source_path_escape",
            Self::SourceLimitExceeded => "source_limit_exceeded",
            Self::DecodeRetryTransient => "decode_retry_transient",
            Self::DecodeRecordPermanent => "decode_record_permanent",
            Self::DecodeStreamFatal => "decode_stream_fatal",
        }
    }

    fn event_tag(self) -> u8 {
        match self {
            Self::SourceRetryTransient => 1,
            Self::SourceUnstable => 2,
            Self::SourceDatabase => 3,
            Self::SourceIo => 4,
            Self::SourceInvalidConfiguration => 5,
            Self::SourceInvalidCursor => 6,
            Self::SourcePathEscape => 7,
            Self::SourceLimitExceeded => 8,
            Self::DecodeRetryTransient => 9,
            Self::DecodeRecordPermanent => 10,
            Self::DecodeStreamFatal => 11,
        }
    }
}

/// Last successfully admitted native position known before an object error.
/// A missing object or a failure before its first stable read has no position,
/// but still carries generation one rather than inventing a zero sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedSourceObjectErrorProvenance {
    pub generation: u64,
    pub last_successful_position: Option<CoveragePosition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedSourceObjectRetryState {
    RetryScheduled {
        failed_attempts: u32,
        max_attempts: u32,
        retry_after_ms: u64,
    },
    RetryExhausted {
        failed_attempts: u32,
        max_attempts: u32,
    },
    NotRetryable {
        failed_attempts: u32,
    },
}

impl ScopedSourceObjectRetryState {
    fn failed_attempts(self) -> u32 {
        match self {
            Self::RetryScheduled {
                failed_attempts, ..
            }
            | Self::RetryExhausted {
                failed_attempts, ..
            }
            | Self::NotRetryable { failed_attempts } => failed_attempts,
        }
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::RetryExhausted { .. } | Self::NotRetryable { .. }
        )
    }

    fn retry_delay(self) -> Option<Duration> {
        match self {
            Self::RetryScheduled { retry_after_ms, .. } => {
                Some(Duration::from_millis(retry_after_ms))
            }
            Self::RetryExhausted { .. } | Self::NotRetryable { .. } => None,
        }
    }

    fn event_tag(self) -> u8 {
        match self {
            Self::RetryScheduled { .. } => 1,
            Self::RetryExhausted { .. } => 2,
            Self::NotRetryable { .. } => 3,
        }
    }
}

/// One immutable retry/error transition for an exact declared relation. The
/// source coordinate and coverage position are canonical common identities;
/// the relation id is declarative metadata rather than a native locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedSourceObjectError {
    pub error_contract_version: u32,
    pub relation_id: Arc<str>,
    pub source: ScopedSourceObjectIdentity,
    pub scope_epoch: u64,
    pub failure_code: ScopedSourceObjectFailureCode,
    pub provenance: ScopedSourceObjectErrorProvenance,
    pub retry: ScopedSourceObjectRetryState,
}

impl ScopedSourceObjectError {
    fn validate(&self) -> bool {
        if self.error_contract_version != SCOPED_SOURCE_OBJECT_ERROR_CONTRACT_VERSION
            || validate_relation_id(&self.relation_id).is_err()
            || self.scope_epoch == 0
            || self.provenance.generation == 0
            || self
                .provenance
                .last_successful_position
                .as_ref()
                .is_some_and(|position| {
                    position.kind != CoveragePositionKind::AppendCursor
                        || position.monotonic_order.is_none()
                })
        {
            return false;
        }
        match self.retry {
            ScopedSourceObjectRetryState::RetryScheduled {
                failed_attempts,
                max_attempts,
                retry_after_ms,
            } => {
                self.failure_code.is_retryable()
                    && failed_attempts > 0
                    && failed_attempts < max_attempts
                    && max_attempts
                        <= ScopedObservationSourceOwnerRetryPolicy::MAX_TRANSIENT_ATTEMPTS
                    && retry_after_ms > 0
                    && retry_after_ms
                        <= u64::try_from(
                            ScopedObservationSourceOwnerRetryPolicy::MAX_RETRY_DELAY.as_millis(),
                        )
                        .expect("the bounded retry duration fits u64 milliseconds")
            }
            ScopedSourceObjectRetryState::RetryExhausted {
                failed_attempts,
                max_attempts,
            } => {
                self.failure_code.is_retryable()
                    && failed_attempts == max_attempts
                    && max_attempts > 0
                    && max_attempts
                        <= ScopedObservationSourceOwnerRetryPolicy::MAX_TRANSIENT_ATTEMPTS
            }
            ScopedSourceObjectRetryState::NotRetryable { failed_attempts } => {
                !self.failure_code.is_retryable() && failed_attempts > 0
            }
        }
    }
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
    access_pass_id: u64,
    /// Host observation time supplied by the trusted source-driver request.
    /// It is delivery metadata and never participates in event identity.
    pub observed_at: i64,
    pub phase: ScopedAppendDeliveryPhase,
    pub reset_before_items: Option<ScopedAppendReset>,
    pub presence_change: Option<ScopedAppendPresenceChange>,
    pub object_present: bool,
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
    access_pass_id: u64,
    pass_evidence: ScopedCoveragePassEvidence,
    coverage: ScopedOfferedDecodeCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedCoveragePassEvidence {
    AccessAttempt,
    RetainedObjectError,
}

#[derive(Clone, PartialEq, Eq)]
struct ScopedCoverageMembershipIdentity {
    relation_id: Arc<str>,
    stream_key: Arc<[u8]>,
    object_key: Arc<[u8]>,
    coverage_domains: Vec<CoverageDomain>,
}

/// Two bounded internal capacity domains multiplexed by one admission ordinal.
/// Coverage membership is a one-to-one accounting of exact host-authorized
/// known-object relations: two semantic objects cannot claim one relation.
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
    offered_access_pass_ids: BTreeMap<ScopedSourceObjectIdentity, u64>,
    offered_pass_evidence: BTreeMap<ScopedSourceObjectIdentity, ScopedCoveragePassEvidence>,
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
            offered_access_pass_ids: BTreeMap::new(),
            offered_pass_evidence: BTreeMap::new(),
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
        if source_is_new
            && self
                .known_coverage_objects
                .values()
                .any(|known| known.relation_id.as_ref() == membership_identity.relation_id.as_ref())
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
            observation.access_pass_id,
            ScopedCoveragePassEvidence::AccessAttempt,
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
        access_pass_id: u64,
        pass_evidence: ScopedCoveragePassEvidence,
        coverage: ScopedOfferedDecodeCoverage,
    ) {
        if through_lane_ordinal <= self.offered_lane_ordinal {
            self.offered_access_pass_ids
                .insert(coverage.source.clone(), access_pass_id);
            self.offered_pass_evidence
                .insert(coverage.source.clone(), pass_evidence);
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
            pending.access_pass_id = access_pass_id;
            pending.pass_evidence = pass_evidence;
            pending.coverage = coverage;
            return;
        }
        self.pending_coverage_updates
            .push_back(PendingScopedCoverageUpdate {
                through_lane_ordinal,
                access_pass_id,
                pass_evidence,
                coverage,
            });
    }

    fn record_object_error_coverage(
        &mut self,
        object: &ScopedKnownAppendObject,
        access_pass_id: u64,
        error: &ScopedSourceObjectError,
        access_attempted: bool,
    ) -> Result<(), ScopedAdmissionError> {
        let membership = object.coverage_membership_identity();
        if error.source != object.source
            || error.relation_id.as_ref() != membership.relation_id.as_ref()
            || self.known_coverage_objects.get(&object.source) != Some(&membership)
        {
            return Err(ScopedAdmissionError::InvalidCoverage);
        }
        let coverage = object
            .prepare_object_error_coverage(error)
            .map_err(|()| ScopedAdmissionError::InvalidCoverage)?;
        self.stage_coverage_update(
            self.offered_lane_ordinal,
            access_pass_id,
            if access_attempted {
                ScopedCoveragePassEvidence::AccessAttempt
            } else {
                ScopedCoveragePassEvidence::RetainedObjectError
            },
            coverage,
        );
        Ok(())
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
            self.offered_access_pass_ids
                .insert(pending.coverage.source.clone(), pending.access_pass_id);
            self.offered_pass_evidence
                .insert(pending.coverage.source.clone(), pending.pass_evidence);
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

    fn relation_pass_evidence(
        &self,
        relation_id: &str,
        access_pass_id: u64,
    ) -> Option<ScopedCoveragePassEvidence> {
        self.known_coverage_objects
            .iter()
            .find(|(_, membership)| membership.relation_id.as_ref() == relation_id)
            .and_then(|(source, _)| {
                (self.offered_access_pass_ids.get(source) == Some(&access_pass_id))
                    .then(|| self.offered_pass_evidence.get(source).copied())
                    .flatten()
            })
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
    pub replacement_snapshot_digest: ScopedReplacementSnapshotDigest,
    pub family_manifest: Vec<ScopedReplacementFamilyManifest>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedObserverFailureReason {
    NativeWatcherRecoveryExhausted,
    NativeWatcherRoutingFailed,
    InternalControlFailure,
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
    pub baseline_snapshot_digest: ScopedReplacementSnapshotDigest,
    pub reason: ScopedResyncReason,
    pub discarded_semantic_events: u64,
    pub discarded_source_controls: u64,
    pub discarded_retained_native_bytes: u64,
}

/// Terminal attachment-local failure. Like continuity invalidation, this
/// control explicitly accounts for and supersedes every not-yet-delivered
/// value, but no later epoch may restore this attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedObserverFailure {
    pub root: ScopedObservationRootIdentity,
    pub failed_scope_epoch: u64,
    pub control_sequence: u64,
    pub last_contiguous_sequence: u64,
    pub phase: ScopedAppendDeliveryPhase,
    pub reason: ScopedObserverFailureReason,
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
    pub baseline_snapshot_digest: ScopedReplacementSnapshotDigest,
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
    SourceObjectError {
        source: ScopedSourceObjectIdentity,
        observed_at: i64,
        event_id: ScopedObservationEventId,
        error: Arc<ScopedSourceObjectError>,
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
    ObserverFailed {
        source: ScopedSourceObjectIdentity,
        observed_at: i64,
        event_id: ScopedObservationEventId,
        failure: Arc<ScopedObserverFailure>,
    },
}

impl ScopedProjectedObservation {
    pub fn event_id(&self) -> ScopedObservationEventId {
        match self {
            Self::SourcePresence { event_id, .. }
            | Self::SourceReset { event_id, .. }
            | Self::SourceObjectError { event_id, .. } => *event_id,
            Self::UsageV2 { event, .. } => event.event_id,
            Self::ObserverBootstrapComplete { event_id, .. } => *event_id,
            Self::ObserverResyncRequired { event_id, .. } => *event_id,
            Self::ObserverResyncStarted { event_id, .. } => *event_id,
            Self::ObserverResyncComplete { event_id, .. } => *event_id,
            Self::ObserverFailed { event_id, .. } => *event_id,
        }
    }

    pub fn semantic_revision_ref(&self) -> Option<SemanticRevisionRef> {
        match self {
            Self::UsageV2 { event, .. } => Some(event.semantic_revision_ref),
            Self::SourcePresence { .. }
            | Self::SourceReset { .. }
            | Self::SourceObjectError { .. }
            | Self::ObserverBootstrapComplete { .. }
            | Self::ObserverResyncRequired { .. }
            | Self::ObserverResyncStarted { .. }
            | Self::ObserverResyncComplete { .. }
            | Self::ObserverFailed { .. } => None,
        }
    }

    pub fn phase(&self) -> ScopedAppendDeliveryPhase {
        match self {
            Self::SourcePresence { phase, .. } | Self::SourceReset { phase, .. } => *phase,
            Self::SourceObjectError { .. } => ScopedAppendDeliveryPhase::Live,
            Self::UsageV2 { event, .. } => event.phase,
            Self::ObserverBootstrapComplete { .. } => ScopedAppendDeliveryPhase::Bootstrap,
            Self::ObserverResyncRequired { .. } => ScopedAppendDeliveryPhase::Live,
            Self::ObserverResyncStarted { .. } => ScopedAppendDeliveryPhase::Correction,
            Self::ObserverResyncComplete { .. } => ScopedAppendDeliveryPhase::Correction,
            Self::ObserverFailed { failure, .. } => failure.phase,
        }
    }

    pub fn source(&self) -> &ScopedSourceObjectIdentity {
        match self {
            Self::SourcePresence { source, .. }
            | Self::SourceReset { source, .. }
            | Self::SourceObjectError { source, .. } => source,
            Self::UsageV2 { event, .. } => &event.source.object,
            Self::ObserverBootstrapComplete { source, .. } => source,
            Self::ObserverResyncRequired { source, .. } => source,
            Self::ObserverResyncStarted { source, .. } => source,
            Self::ObserverResyncComplete { source, .. } => source,
            Self::ObserverFailed { source, .. } => source,
        }
    }

    pub fn observed_at(&self) -> i64 {
        match self {
            Self::SourcePresence { observed_at, .. } | Self::SourceReset { observed_at, .. } => {
                *observed_at
            }
            Self::SourceObjectError { observed_at, .. } => *observed_at,
            Self::UsageV2 { event, .. } => event.observed_at,
            Self::ObserverBootstrapComplete { observed_at, .. } => *observed_at,
            Self::ObserverResyncRequired { observed_at, .. } => *observed_at,
            Self::ObserverResyncStarted { observed_at, .. } => *observed_at,
            Self::ObserverResyncComplete { observed_at, .. } => *observed_at,
            Self::ObserverFailed { observed_at, .. } => *observed_at,
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
    SourceObjectError {
        error: Arc<ScopedSourceObjectError>,
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
    ObserverFailed {
        failure: Arc<ScopedObserverFailure>,
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

/// Attachment-local acknowledgement for the consumer-ready drain. This is not
/// a semantic identity and must never be serialized or retained across an
/// observer attachment. The private authority prevents an otherwise identical
/// replay from another observer from acknowledging this drain.
#[derive(Clone)]
pub struct ScopedObservationApplicationReceipt {
    authority: Arc<ScopedObservationApplicationAuthority>,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: ScopedObservationEventId,
}

impl std::fmt::Debug for ScopedObservationApplicationReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationApplicationReceipt")
            .field("observer_sequence", &self.observer_sequence)
            .field("scope_epoch", &self.scope_epoch)
            .field("event_id", &self.event_id)
            .finish_non_exhaustive()
    }
}

impl ScopedObservationApplicationReceipt {
    pub fn observer_sequence(&self) -> u64 {
        self.observer_sequence
    }

    pub fn scope_epoch(&self) -> u64 {
        self.scope_epoch
    }

    pub fn event_id(&self) -> ScopedObservationEventId {
        self.event_id
    }

    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.authority, &other.authority)
            && self.observer_sequence == other.observer_sequence
            && self.scope_epoch == other.scope_epoch
            && self.event_id == other.event_id
    }
}

#[derive(Debug)]
struct ScopedObservationApplicationAuthority;

/// Unforgeable identity shared only by one authorized host and the consumer
/// drain it constructs. Root identity alone is insufficient because two
/// simultaneous attachments may intentionally observe the same native scope.
#[derive(Debug)]
struct ScopedObservationAttachmentAuthority;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationCloseState {
    pub close_requested: bool,
    pub active_operations: u64,
    /// Subset of `active_operations` owned by installed watcher tasks. Close
    /// cannot complete until every watcher observes cancellation and drops its
    /// registration.
    pub active_watcher_tasks: u64,
    pub consumer_drain_pending: bool,
    pub complete: bool,
}

#[derive(Debug, Default)]
struct ScopedObservationAttachmentLifecycleState {
    close_requested: bool,
    active_operations: u64,
    active_watcher_tasks: u64,
    consumer_drain_opened: bool,
    consumer_drain_closed: bool,
    consumer_event_completion: Option<Weak<ScopedObservationEventCompletion>>,
}

#[derive(Debug)]
struct ScopedObservationAttachmentLifecycle {
    state: Mutex<ScopedObservationAttachmentLifecycleState>,
    idle: Condvar,
    async_changed: tokio::sync::watch::Sender<ScopedObservationCloseState>,
}

impl Default for ScopedObservationAttachmentLifecycle {
    fn default() -> Self {
        let state = ScopedObservationAttachmentLifecycleState::default();
        let (async_changed, _) = tokio::sync::watch::channel(close_state_snapshot(&state));
        Self {
            state: Mutex::new(state),
            idle: Condvar::new(),
            async_changed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedObservationOperationStartError {
    Closing,
    CapacityExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedObservationOperationKind {
    Runtime,
    Watcher,
}

impl ScopedObservationAttachmentLifecycle {
    fn lock_state(&self) -> MutexGuard<'_, ScopedObservationAttachmentLifecycleState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn start_operation(
        self: &Arc<Self>,
        kind: ScopedObservationOperationKind,
    ) -> Result<ScopedObservationOperationGuard, ScopedObservationOperationStartError> {
        let mut state = self.lock_state();
        if state.close_requested {
            return Err(ScopedObservationOperationStartError::Closing);
        }
        let active_operations = state
            .active_operations
            .checked_add(1)
            .ok_or(ScopedObservationOperationStartError::CapacityExhausted)?;
        let active_watcher_tasks = match kind {
            ScopedObservationOperationKind::Runtime => state.active_watcher_tasks,
            ScopedObservationOperationKind::Watcher => state
                .active_watcher_tasks
                .checked_add(1)
                .ok_or(ScopedObservationOperationStartError::CapacityExhausted)?,
        };
        state.active_operations = active_operations;
        state.active_watcher_tasks = active_watcher_tasks;
        self.async_changed
            .send_replace(close_state_snapshot(&state));
        Ok(ScopedObservationOperationGuard {
            lifecycle: Arc::clone(self),
            kind,
            active: true,
        })
    }

    fn finish_operation(&self, kind: ScopedObservationOperationKind) {
        let mut state = self.lock_state();
        state.active_operations = state
            .active_operations
            .checked_sub(1)
            .expect("scoped attachment operation accounting cannot underflow");
        if kind == ScopedObservationOperationKind::Watcher {
            state.active_watcher_tasks = state
                .active_watcher_tasks
                .checked_sub(1)
                .expect("scoped watcher task accounting cannot underflow");
        }
        self.async_changed
            .send_replace(close_state_snapshot(&state));
        if close_is_complete(&state) {
            self.idle.notify_all();
        }
    }

    fn open_consumer_drain(
        &self,
        event_completion: &Arc<ScopedObservationEventCompletion>,
    ) -> Result<(), ScopedObservationOperationStartError> {
        let mut state = self.lock_state();
        if state.close_requested {
            return Err(ScopedObservationOperationStartError::Closing);
        }
        if state.consumer_drain_opened {
            return Err(ScopedObservationOperationStartError::CapacityExhausted);
        }
        state.consumer_drain_opened = true;
        state.consumer_drain_closed = false;
        state.consumer_event_completion = Some(Arc::downgrade(event_completion));
        self.async_changed
            .send_replace(close_state_snapshot(&state));
        Ok(())
    }

    fn close_consumer_drain(&self) {
        let mut state = self.lock_state();
        if state.consumer_drain_opened {
            state.consumer_drain_closed = true;
        }
        state.consumer_event_completion = None;
        self.async_changed
            .send_replace(close_state_snapshot(&state));
        if close_is_complete(&state) {
            self.idle.notify_all();
        }
    }

    fn begin_close(self: &Arc<Self>) -> ScopedObservationCloseBarrier {
        let mut state = self.lock_state();
        state.close_requested = true;
        let event_completion = state
            .consumer_event_completion
            .as_ref()
            .and_then(Weak::upgrade);
        self.async_changed
            .send_replace(close_state_snapshot(&state));
        // Wake both close-barrier waiters and watcher tasks waiting for their
        // cancellation signal. Barrier waiters re-check full completion;
        // watchers must wake before they can release the operations that make
        // completion possible.
        self.idle.notify_all();
        drop(state);
        if let Some(event_completion) = event_completion {
            event_completion.close();
        }
        ScopedObservationCloseBarrier {
            lifecycle: Arc::clone(self),
        }
    }

    fn is_closing(&self) -> bool {
        self.lock_state().close_requested
    }

    fn wait_for_close_request(&self) {
        let mut state = self.lock_state();
        while !state.close_requested {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    async fn wait_for_close_request_async(&self) {
        let mut changed = self.async_changed.subscribe();
        loop {
            let state = *changed.borrow_and_update();
            if state.close_requested {
                return;
            }
            changed
                .changed()
                .await
                .expect("scoped lifecycle retains its async notification sender");
        }
    }

    fn snapshot(&self) -> ScopedObservationCloseState {
        close_state_snapshot(&self.lock_state())
    }

    fn wait(&self) -> ScopedObservationCloseState {
        let mut state = self.lock_state();
        while !close_is_complete(&state) {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        close_state_snapshot(&state)
    }

    async fn wait_async(&self) -> ScopedObservationCloseState {
        let mut changed = self.async_changed.subscribe();
        loop {
            let state = *changed.borrow_and_update();
            if state.complete {
                return state;
            }
            changed
                .changed()
                .await
                .expect("scoped lifecycle retains its async notification sender");
        }
    }
}

fn close_is_complete(state: &ScopedObservationAttachmentLifecycleState) -> bool {
    state.close_requested
        && state.active_operations == 0
        && (!state.consumer_drain_opened || state.consumer_drain_closed)
}

fn close_state_snapshot(
    state: &ScopedObservationAttachmentLifecycleState,
) -> ScopedObservationCloseState {
    ScopedObservationCloseState {
        close_requested: state.close_requested,
        active_operations: state.active_operations,
        active_watcher_tasks: state.active_watcher_tasks,
        consumer_drain_pending: state.consumer_drain_opened && !state.consumer_drain_closed,
        complete: close_is_complete(state),
    }
}

/// Internal synchronous substrate for the future async `close()` facade. The
/// barrier is attachment-owned and becomes complete only after all registered
/// operations and the sole consumer drain acknowledge cancellation.
#[derive(Debug, Clone)]
pub struct ScopedObservationCloseBarrier {
    lifecycle: Arc<ScopedObservationAttachmentLifecycle>,
}

impl ScopedObservationCloseBarrier {
    pub fn state(&self) -> ScopedObservationCloseState {
        self.lifecycle.snapshot()
    }

    pub fn wait(&self) -> ScopedObservationCloseState {
        self.lifecycle.wait()
    }

    /// Executor-friendly future for portable hosts. The retained watch state
    /// makes completion visible even when close finishes before first poll.
    pub async fn wait_async(&self) -> ScopedObservationCloseState {
        self.lifecycle.wait_async().await
    }
}

#[derive(Debug)]
struct ScopedObservationOperationGuard {
    lifecycle: Arc<ScopedObservationAttachmentLifecycle>,
    kind: ScopedObservationOperationKind,
    active: bool,
}

impl Drop for ScopedObservationOperationGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            self.lifecycle.finish_operation(self.kind);
        }
    }
}

/// Attachment-owned registration held by one watcher task. Requesting close
/// makes `cancellation_requested()` sticky; dropping the registration is the
/// watcher's acknowledgement that no callback or source hint can run again.
/// The registration is deliberately non-cloneable so one task owns exactly
/// one close-barrier obligation.
#[derive(Debug)]
#[must_use = "the watcher task must retain its registration until it has stopped"]
pub struct ScopedObservationWatcherRegistration {
    lifecycle: Arc<ScopedObservationAttachmentLifecycle>,
    _operation: ScopedObservationOperationGuard,
}

impl ScopedObservationWatcherRegistration {
    pub fn cancellation_requested(&self) -> bool {
        self.lifecycle.is_closing()
    }

    /// Block the watcher task until attachment close requests cancellation.
    /// Returning does not acknowledge shutdown: the task must first stop its
    /// native watcher/callbacks, then drop this registration.
    pub fn wait_for_cancellation(&self) {
        self.lifecycle.wait_for_close_request();
    }

    /// Await attachment cancellation without occupying a runtime worker.
    /// Dropping this registration after the future resolves remains the
    /// watcher's close-barrier acknowledgement.
    pub async fn wait_for_cancellation_async(&self) {
        self.lifecycle.wait_for_close_request_async().await;
    }
}

struct ScopedObservationWatcherStartupState {
    ordering: WatchBeforeScan,
    backend_installed: bool,
    reconcile_pass_active: bool,
}

/// Attachment-owned synchronous orchestration for the watcher/bootstrap race.
/// The portable async facade will own this value beside the native watcher
/// handle. Keeping the watcher registration inside it makes close wait until
/// callbacks have stopped and this coordinator is dropped.
pub struct ScopedObservationWatcherOrchestrator {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    host_state: Arc<ScopedObservationAccessState>,
    startup: Arc<Mutex<ScopedObservationWatcherStartupState>>,
    registration: ScopedObservationWatcherRegistration,
}

impl std::fmt::Debug for ScopedObservationWatcherOrchestrator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationWatcherOrchestrator")
            .field("phase", &self.phase())
            .field(
                "cancellation_requested",
                &self.registration.cancellation_requested(),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedObservationWatcherPhase {
    InstallingWatcher,
    WatcherInstalled,
    InitialScan,
    ReconcilePending,
    Reconciling,
    Live { offered_through_sequence: u64 },
}

impl From<StartupPhase> for ScopedObservationWatcherPhase {
    fn from(phase: StartupPhase) -> Self {
        match phase {
            StartupPhase::Discovered | StartupPhase::WatchRegistered => Self::WatcherInstalled,
            StartupPhase::Scanning => Self::InitialScan,
            StartupPhase::Replaying => Self::ReconcilePending,
            StartupPhase::Reconciling => Self::Reconciling,
            StartupPhase::Live { commit_seq } => Self::Live {
                offered_through_sequence: commit_seq,
            },
        }
    }
}

fn scoped_watcher_phase(
    startup: &ScopedObservationWatcherStartupState,
) -> ScopedObservationWatcherPhase {
    if startup.backend_installed {
        startup.ordering.phase().into()
    } else {
        ScopedObservationWatcherPhase::InstallingWatcher
    }
}

impl Drop for ScopedObservationWatcherOrchestrator {
    fn drop(&mut self) {
        let installed = self
            .startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .backend_installed;
        if !installed {
            self.host_state
                .watcher_orchestrator_opened
                .store(false, Ordering::Release);
        }
    }
}

#[derive(Debug)]
pub enum ScopedObservationWatcherHintAction {
    Buffered(HintEnqueue),
    PollRequested {
        hint: DirtyHint,
        ticket: ScopedObservationPollTicket,
    },
}

pub enum ScopedObservationStartupReconcileAction {
    Reconcile(Box<ScopedObservationStartupReconcilePass>),
    CaughtUp,
}

/// Typestate owner for the one exact-scope initial scan. Dropping an
/// unfinished attempt rolls the ordering state back to `WatcherInstalled`
/// without discarding hints captured while it ran.
pub struct ScopedObservationInitialScan {
    startup: Arc<Mutex<ScopedObservationWatcherStartupState>>,
    access_pass: Option<ScopedObservationAccessPass>,
    completed: bool,
}

impl ScopedObservationInitialScan {
    pub fn access_pass(&self) -> &ScopedObservationAccessPass {
        self.access_pass
            .as_ref()
            .expect("an unfinished initial scan retains its scoped access pass")
    }
}

impl Drop for ScopedObservationInitialScan {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        drop(self.access_pass.take());
        let mut startup = self
            .startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if startup.ordering.phase() == StartupPhase::Scanning {
            let _ = startup.ordering.abort_scan();
        }
    }
}

/// One bounded whole-scope pass triggered by hints captured behind the
/// bootstrap barrier. Its hints are automatically restored if the pass is
/// dropped or cannot publish complete offered coverage.
pub struct ScopedObservationStartupReconcilePass {
    startup: Arc<Mutex<ScopedObservationWatcherStartupState>>,
    hints: Vec<DirtyHint>,
    access_pass: Option<ScopedObservationAccessPass>,
    completed: bool,
}

impl ScopedObservationStartupReconcilePass {
    pub fn hints(&self) -> &[DirtyHint] {
        &self.hints
    }

    pub fn access_pass(&self) -> &ScopedObservationAccessPass {
        self.access_pass
            .as_ref()
            .expect("an unfinished startup reconciliation retains its scoped access pass")
    }
}

impl Drop for ScopedObservationStartupReconcilePass {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        drop(self.access_pass.take());
        let mut startup = self
            .startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for hint in self.hints.drain(..) {
            let _ = startup.ordering.push_hint(hint);
        }
        startup.reconcile_pass_active = false;
    }
}

#[derive(Debug)]
pub struct ScopedObservationYieldedEnvelope {
    pub envelope: ScopedObservationEnvelope,
    application_receipt: ScopedObservationApplicationReceipt,
}

impl ScopedObservationYieldedEnvelope {
    pub fn application_receipt(&self) -> &ScopedObservationApplicationReceipt {
        &self.application_receipt
    }
}

/// Application progress belongs to the consumer-ready helper, not to engine
/// readiness. A sequence may jump when an explicit continuity control
/// supersedes undelivered backlog; it still names the last envelope yielded and
/// acknowledged through this one logical drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationAppliedState {
    pub delivered_through_sequence: u64,
    pub applied_through_sequence: u64,
    pub applied_scope_epoch: Option<u64>,
    pub pending_sequence: Option<u64>,
    pub bootstrap_barrier_sequence: Option<u64>,
    pub resync_barrier_sequence: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationDrainError {
    #[error("scoped observation consumer drain is closed")]
    Closed,
    #[error("scoped observation consumer operation accounting is exhausted")]
    OperationCapacityExhausted,
    #[error("scoped observation consumer must apply the yielded envelope before draining another")]
    ApplicationPending,
    #[error("scoped observation envelope mapping failed: {0}")]
    Envelope(ScopedEnvelopeError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationApplicationError {
    #[error("scoped observation consumer drain is closed")]
    Closed,
    #[error("scoped observation consumer operation accounting is exhausted")]
    OperationCapacityExhausted,
    #[error("scoped observation application receipt belongs to another drain")]
    ForeignReceipt,
    #[error("scoped observation application receipt does not match the pending envelope")]
    ReceiptMismatch,
    #[error("scoped observation drain has no pending envelope to acknowledge")]
    NoPendingEnvelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationOpenDrainError {
    #[error("scoped observation host is closed")]
    Closed,
    #[error("scoped observation consumer drain was already opened")]
    AlreadyOpened,
    #[error("scoped observation delivery lane could not be created: {0}")]
    Delivery(ScopedDeliveryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationCloseError {
    #[error("scoped consumer drain belongs to another observer attachment")]
    ForeignDrain,
}

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationConsumerOfferError {
    #[error("scoped consumer drain belongs to another observer attachment")]
    ForeignDrain,
    #[error("scoped observation host or consumer drain is closed")]
    Closed,
    #[error("scoped observation consumer operation accounting is exhausted")]
    OperationCapacityExhausted,
    #[error("scoped observation projection/delivery offer failed: {0}")]
    Offer(#[from] ScopedProjectionDeliveryError),
}

#[derive(Debug, Clone)]
enum ScopedObservationAppliedBoundary {
    None,
    Bootstrap(Arc<ScopedBootstrapBarrier>),
    Resync(Arc<ScopedResyncBarrier>),
}

#[derive(Debug, Clone)]
struct ScopedObservationPendingApplication {
    receipt: ScopedObservationApplicationReceipt,
    boundary: ScopedObservationAppliedBoundary,
}

/// Single-consumer, one-envelope-in-flight drain used by the future
/// consumer-ready SDK helper. Engine `ready()` remains the offered bootstrap
/// barrier; this state advances only after the caller reports that its reducer
/// successfully applied the exact yielded envelope.
pub struct ScopedObservationConsumerDrain {
    mapper: ScopedObservationEnvelopeMapper,
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    lifecycle: Arc<ScopedObservationAttachmentLifecycle>,
    authority: Arc<ScopedObservationApplicationAuthority>,
    delivery: ScopedObservationDeliveryLane,
    delivered_through_sequence: u64,
    applied_through_sequence: u64,
    applied_scope_epoch: Option<u64>,
    pending: Option<ScopedObservationPendingApplication>,
    last_applied: Option<ScopedObservationApplicationReceipt>,
    bootstrap_barrier: Option<Arc<ScopedBootstrapBarrier>>,
    resync_barrier: Option<Arc<ScopedResyncBarrier>>,
    lifecycle_registered: bool,
    closed: bool,
}

impl ScopedObservationConsumerDrain {
    fn new(
        mapper: ScopedObservationEnvelopeMapper,
        attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
        lifecycle: Arc<ScopedObservationAttachmentLifecycle>,
        limits: ScopedObservationDeliveryLimits,
    ) -> Result<Self, ScopedDeliveryError> {
        let authority = Arc::new(ScopedObservationApplicationAuthority);
        Ok(Self {
            mapper,
            attachment_authority,
            lifecycle,
            authority,
            delivery: ScopedObservationDeliveryLane::new(limits)?,
            delivered_through_sequence: 0,
            applied_through_sequence: 0,
            applied_scope_epoch: None,
            pending: None,
            last_applied: None,
            bootstrap_barrier: None,
            resync_barrier: None,
            lifecycle_registered: false,
            closed: false,
        })
    }

    /// Yield one mapped envelope. The next envelope remains unavailable until
    /// the caller acknowledges successful application of this exact delivery.
    /// Mapping is validated against a clone before dequeue, so a mapper failure
    /// cannot consume or advance the delivery boundary.
    pub fn next(
        &mut self,
    ) -> Result<Option<ScopedObservationYieldedEnvelope>, ScopedObservationDrainError> {
        if self.closed {
            return Err(ScopedObservationDrainError::Closed);
        }
        let _operation = match self
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
        {
            Ok(operation) => operation,
            Err(ScopedObservationOperationStartError::Closing) => {
                self.close();
                return Err(ScopedObservationDrainError::Closed);
            }
            Err(ScopedObservationOperationStartError::CapacityExhausted) => {
                return Err(ScopedObservationDrainError::OperationCapacityExhausted);
            }
        };
        if self.pending.is_some() {
            return Err(ScopedObservationDrainError::ApplicationPending);
        }
        let Some(preview) = self.delivery.preview_next() else {
            return Ok(None);
        };
        let preview_sequence = preview.observer_sequence;
        let preview_epoch = preview.scope_epoch;
        let preview_event_id = preview.event_id;
        let envelope = self
            .mapper
            .map(preview)
            .map_err(ScopedObservationDrainError::Envelope)?;
        let delivered = self
            .delivery
            .dequeue_next()
            .expect("validated scoped delivery preview remains queued");
        debug_assert_eq!(delivered.observer_sequence, preview_sequence);
        debug_assert_eq!(delivered.scope_epoch, preview_epoch);
        debug_assert_eq!(delivered.event_id, preview_event_id);

        let boundary = match &envelope.event {
            ScopedObservationEvent::ObserverBootstrapComplete { barrier } => {
                ScopedObservationAppliedBoundary::Bootstrap(Arc::clone(barrier))
            }
            ScopedObservationEvent::ObserverResyncComplete { barrier } => {
                ScopedObservationAppliedBoundary::Resync(Arc::clone(barrier))
            }
            ScopedObservationEvent::SourcePresence { .. }
            | ScopedObservationEvent::SourceReset { .. }
            | ScopedObservationEvent::SourceObjectError { .. }
            | ScopedObservationEvent::UsageV2 { .. }
            | ScopedObservationEvent::ObserverResyncRequired { .. }
            | ScopedObservationEvent::ObserverResyncStarted { .. }
            | ScopedObservationEvent::ObserverFailed { .. } => {
                ScopedObservationAppliedBoundary::None
            }
        };
        let receipt = ScopedObservationApplicationReceipt {
            authority: Arc::clone(&self.authority),
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
            event_id: envelope.event_id,
        };
        self.delivered_through_sequence = envelope.observer_sequence;
        self.pending = Some(ScopedObservationPendingApplication {
            receipt: receipt.clone(),
            boundary,
        });
        Ok(Some(ScopedObservationYieldedEnvelope {
            envelope,
            application_receipt: receipt,
        }))
    }

    /// Advance the consumer-owned boundary after its reducer has applied the
    /// yielded envelope. Retrying the latest successful acknowledgement is a
    /// no-op; foreign, stale, or mismatched receipts change no state.
    pub fn acknowledge_applied(
        &mut self,
        receipt: &ScopedObservationApplicationReceipt,
    ) -> Result<ScopedObservationAppliedState, ScopedObservationApplicationError> {
        if self.closed {
            return Err(ScopedObservationApplicationError::Closed);
        }
        let _operation = match self
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
        {
            Ok(operation) => operation,
            Err(ScopedObservationOperationStartError::Closing) => {
                self.close();
                return Err(ScopedObservationApplicationError::Closed);
            }
            Err(ScopedObservationOperationStartError::CapacityExhausted) => {
                return Err(ScopedObservationApplicationError::OperationCapacityExhausted);
            }
        };
        if !Arc::ptr_eq(&self.authority, &receipt.authority) {
            return Err(ScopedObservationApplicationError::ForeignReceipt);
        }
        if self
            .last_applied
            .as_ref()
            .is_some_and(|last| last.matches(receipt))
        {
            return Ok(self.state());
        }
        let pending = self
            .pending
            .as_ref()
            .ok_or(ScopedObservationApplicationError::NoPendingEnvelope)?;
        if !pending.receipt.matches(receipt) {
            return Err(ScopedObservationApplicationError::ReceiptMismatch);
        }
        let pending = self
            .pending
            .take()
            .expect("validated scoped application remains pending");
        self.applied_through_sequence = pending.receipt.observer_sequence;
        self.applied_scope_epoch = Some(pending.receipt.scope_epoch);
        match pending.boundary {
            ScopedObservationAppliedBoundary::None => {}
            ScopedObservationAppliedBoundary::Bootstrap(barrier) => {
                self.bootstrap_barrier = Some(barrier);
            }
            ScopedObservationAppliedBoundary::Resync(barrier) => {
                self.resync_barrier = Some(barrier);
            }
        }
        self.last_applied = Some(pending.receipt);
        Ok(self.state())
    }

    pub fn state(&self) -> ScopedObservationAppliedState {
        ScopedObservationAppliedState {
            delivered_through_sequence: self.delivered_through_sequence,
            applied_through_sequence: self.applied_through_sequence,
            applied_scope_epoch: self.applied_scope_epoch,
            pending_sequence: self
                .pending
                .as_ref()
                .map(|pending| pending.receipt.observer_sequence),
            bootstrap_barrier_sequence: self
                .bootstrap_barrier
                .as_ref()
                .map(|barrier| barrier.barrier_sequence),
            resync_barrier_sequence: self
                .resync_barrier
                .as_ref()
                .map(|barrier| barrier.barrier_sequence),
        }
    }

    pub fn consumer_bootstrap_barrier(&self) -> Option<Arc<ScopedBootstrapBarrier>> {
        self.bootstrap_barrier.as_ref().map(Arc::clone)
    }

    pub fn consumer_resync_barrier(&self) -> Option<Arc<ScopedResyncBarrier>> {
        self.resync_barrier.as_ref().map(Arc::clone)
    }

    /// Cloneable lost-wakeup-safe notification handle for the future async
    /// iterator bridge. The bridge checks `next()`, snapshots this handle while
    /// it still owns the drain, then waits for a later offered sequence or
    /// close before checking `next()` again.
    pub fn event_waiter(&self) -> ScopedObservationEventWaiter {
        self.delivery.event_waiter()
    }

    /// Cloneable producer-side capacity notification. Source owners capture
    /// this state under the same drain lock before offering, then await only
    /// after releasing that lock when delivery reports bounded backpressure.
    pub fn delivery_capacity_waiter(&self) -> ScopedObservationDeliveryCapacityWaiter {
        self.delivery.capacity_waiter()
    }

    /// Producer-side access remains inside the crate-private observer runtime.
    /// Owning rather than borrowing the lane prevents a consumer drain from
    /// being paired with a second attachment or epoch queue.
    pub fn delivery_lane(&self) -> &ScopedObservationDeliveryLane {
        &self.delivery
    }

    fn delivery_lane_mut(&mut self) -> &mut ScopedObservationDeliveryLane {
        &mut self.delivery
    }

    pub fn engine_bootstrap_barrier(&self) -> Option<Arc<ScopedBootstrapBarrier>> {
        self.delivery.bootstrap_barrier()
    }

    pub fn engine_resync_barrier(&self) -> Option<Arc<ScopedResyncBarrier>> {
        self.delivery.resync_barrier()
    }

    /// Cancel the sole delivery/application path, discard all envelopes that
    /// were never applied, and acknowledge the drain side of attachment close.
    /// Applied boundaries remain observable as historical local state, while
    /// pending receipts become unusable.
    pub fn close(&mut self) -> ScopedObservationAppliedState {
        if !self.closed {
            self.closed = true;
            self.pending = None;
            self.delivery.discard_for_close();
            if self.lifecycle_registered {
                self.lifecycle_registered = false;
                self.lifecycle.close_consumer_drain();
            }
        }
        self.state()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

impl Drop for ScopedObservationConsumerDrain {
    fn drop(&mut self) {
        self.close();
    }
}

struct ScopedObservationAsyncRuntimeShared {
    host: ScopedObservationAccessHost,
    drain: Mutex<ScopedObservationConsumerDrain>,
}

impl ScopedObservationAsyncRuntimeShared {
    fn lock_drain(&self) -> MutexGuard<'_, ScopedObservationConsumerDrain> {
        self.drain
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn request_close(&self) -> ScopedObservationCloseBarrier {
        let mut drain = self.lock_drain();
        self.host
            .close_with_consumer(&mut drain)
            .expect("the async runtime retains its attachment-owned consumer drain")
    }

    /// Preserve an already-admitted terminal failure for the event owner. The
    /// check shares the drain lock with failure admission, so a watcher drop
    /// racing `fail_observer` cannot observe a stale non-failed state and then
    /// discard the control by closing the drain.
    fn request_close_unless_observer_failed(&self) {
        let mut drain = self.lock_drain();
        if drain.delivery_lane().state().continuity != ScopedObservationContinuity::Failed {
            self.host
                .close_with_consumer(&mut drain)
                .expect("the async runtime retains its attachment-owned consumer drain");
        }
    }
}

/// Cloneable control/producer handle for the internal portable observer runtime.
/// Every operation that can inspect or mutate the consumer-owned queue enters
/// the same short-held lock used by the sole async event drain. Callers must
/// not await inside `with_attachment`; native access and decode happen outside
/// this boundary and only their bounded offer step enters it.
#[derive(Clone)]
pub struct ScopedObservationAsyncHandle {
    shared: Arc<ScopedObservationAsyncRuntimeShared>,
}

/// Non-cloneable owner of one attachment's mutable source/coverage/reducer
/// epoch. It is registered with the close barrier for its whole lifetime and
/// builds borrowed pass requests only for the duration of one synchronous
/// bounded source attempt.
pub struct ScopedObservationAsyncSourceOwner {
    handle: ScopedObservationAsyncHandle,
    active: ScopedObservationEpochState,
    bindings: Vec<ScopedObservationAppendPassBinding>,
    policy: ScopedObservationSourceOwnerRetryPolicy,
    retry_deadlines: BTreeMap<String, tokio::time::Instant>,
    operation: ScopedObservationOperationGuard,
}

impl std::fmt::Debug for ScopedObservationAsyncSourceOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationAsyncSourceOwner")
            .field("scope_epoch", &self.active.scope_epoch)
            .field("bindings", &self.bindings)
            .field("policy", &self.policy)
            .field("object_error_count", &self.active.object_errors.len())
            .field("scheduled_retry_count", &self.retry_deadlines.len())
            .finish_non_exhaustive()
    }
}

impl ScopedObservationAsyncHandle {
    pub fn host(&self) -> &ScopedObservationAccessHost {
        &self.shared.host
    }

    pub fn with_attachment<T>(
        &self,
        operation: impl FnOnce(&ScopedObservationAccessHost, &mut ScopedObservationConsumerDrain) -> T,
    ) -> T {
        let mut drain = self.shared.lock_drain();
        operation(&self.shared.host, &mut drain)
    }

    /// Deliver the one terminal observer-failure control through this
    /// attachment's dedicated control lane. The first failure wins and is
    /// retained idempotently; close still owns final resource cancellation.
    pub fn fail_observer(
        &self,
        reason: ScopedObserverFailureReason,
        observed_at: i64,
    ) -> Result<Arc<ScopedObserverFailure>, ScopedContinuityError> {
        let mut drain = self.shared.lock_drain();
        if drain.is_closed()
            || self.shared.host.state.closed.load(Ordering::Acquire)
            || self.shared.host.lifecycle.is_closing()
        {
            return Err(ScopedContinuityError::Closed);
        }
        let _operation = self
            .shared
            .host
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
            .map_err(|error| match error {
                ScopedObservationOperationStartError::Closing => ScopedContinuityError::Closed,
                ScopedObservationOperationStartError::CapacityExhausted => {
                    ScopedContinuityError::OperationCapacityExhausted
                }
            })?;
        let failure = drain.delivery_lane_mut().fail_observer(
            &self.shared.host.root_identity,
            reason,
            observed_at,
        )?;
        self.shared.host.state.poll.fail(Arc::clone(&failure));
        self.shared.host.state.ready.fail(Arc::clone(&failure));
        Ok(failure)
    }

    /// Invalidate the attachment-owned live epoch through the same consumer
    /// lane observed by the ordered event owner and automatic source owner.
    /// The source owner wakes on this exact control offer and returns its
    /// intact epoch plus rebind material instead of waiting for another poll.
    pub fn require_resync(
        &self,
        reason: ScopedResyncReason,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncRequired>, ScopedContinuityError> {
        let mut drain = self.shared.lock_drain();
        if drain.is_closed()
            || self.shared.host.state.closed.load(Ordering::Acquire)
            || self.shared.host.lifecycle.is_closing()
        {
            return Err(ScopedContinuityError::Closed);
        }
        let _operation = self
            .shared
            .host
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
            .map_err(|error| match error {
                ScopedObservationOperationStartError::Closing => ScopedContinuityError::Closed,
                ScopedObservationOperationStartError::CapacityExhausted => {
                    ScopedContinuityError::OperationCapacityExhausted
                }
            })?;
        self.shared
            .host
            .require_resync(drain.delivery_lane_mut(), reason, observed_at)
    }

    /// Start the replacement epoch only after the attachment-owned consumer
    /// has delivered the matching invalidation control.
    pub fn begin_resync(
        &self,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncStarted>, ScopedContinuityError> {
        let mut drain = self.shared.lock_drain();
        if drain.is_closed()
            || self.shared.host.state.closed.load(Ordering::Acquire)
            || self.shared.host.lifecycle.is_closing()
        {
            return Err(ScopedContinuityError::Closed);
        }
        let _operation = self
            .shared
            .host
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
            .map_err(|error| match error {
                ScopedObservationOperationStartError::Closing => ScopedContinuityError::Closed,
                ScopedObservationOperationStartError::CapacityExhausted => {
                    ScopedContinuityError::OperationCapacityExhausted
                }
            })?;
        self.shared
            .host
            .begin_resync(drain.delivery_lane_mut(), observed_at)
    }

    /// Create the whole-scope replacement owner against this attachment's
    /// exact delivery lane. The stopped source owner remains the sole source
    /// of the active epoch passed here.
    pub fn open_scope_resync_stage(
        &self,
        active: &mut ScopedObservationEpochState,
    ) -> Result<ScopedObservationScopeReplacementStage, ScopedReplacementStageError> {
        let drain = self.shared.lock_drain();
        if drain.is_closed()
            || self.shared.host.state.closed.load(Ordering::Acquire)
            || self.shared.host.lifecycle.is_closing()
        {
            return Err(ScopedReplacementStageError::Closed);
        }
        let _operation = self
            .shared
            .host
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
            .map_err(|error| match error {
                ScopedObservationOperationStartError::Closing => {
                    ScopedReplacementStageError::Closed
                }
                ScopedObservationOperationStartError::CapacityExhausted => {
                    ScopedReplacementStageError::OperationCapacityExhausted
                }
            })?;
        self.shared
            .host
            .open_scope_resync_stage(active, drain.delivery_lane())
    }

    /// Atomically offer the replacement completion control and transfer its
    /// source, coverage, and reducer state through the attachment-owned lane.
    pub fn offer_scope_resync_complete(
        &self,
        active: &mut ScopedObservationEpochState,
        stage: &mut ScopedObservationScopeReplacementStage,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncBarrier>, ScopedReplacementStageError> {
        let mut drain = self.shared.lock_drain();
        if drain.is_closed()
            || self.shared.host.state.closed.load(Ordering::Acquire)
            || self.shared.host.lifecycle.is_closing()
        {
            return Err(ScopedReplacementStageError::Closed);
        }
        let _operation = self
            .shared
            .host
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
            .map_err(|error| match error {
                ScopedObservationOperationStartError::Closing => {
                    ScopedReplacementStageError::Closed
                }
                ScopedObservationOperationStartError::CapacityExhausted => {
                    ScopedReplacementStageError::OperationCapacityExhausted
                }
            })?;
        self.shared.host.offer_scope_resync_complete(
            active,
            stage,
            drain.delivery_lane_mut(),
            observed_at,
        )
    }

    /// Request cancellation and close the event drain. Watcher/native owners
    /// still acknowledge shutdown by dropping their registered operations;
    /// callers may await the returned barrier without holding the drain lock.
    pub fn request_close(&self) -> ScopedObservationCloseBarrier {
        self.shared.request_close()
    }

    /// Resolve engine-level bootstrap readiness concurrently with the sole
    /// event drain. This proves the offered barrier only; a consumer-ready
    /// helper must also acknowledge the matching completion envelope.
    pub async fn ready(
        &self,
    ) -> Result<ScopedObservationReadyResolution, ScopedObservationPollError> {
        let waiter = self.shared.host.ready_waiter()?;
        Ok(waiter.wait_async().await)
    }

    /// Request one logical exact-scope poll and await its request-local offered
    /// watermark. The watcher/runtime pass driver remains responsible for
    /// reserving and completing the corresponding bounded pass.
    pub async fn poll(
        &self,
    ) -> Result<ScopedObservationPollResolution, ScopedObservationPollError> {
        let ticket = self.shared.host.request_poll()?;
        Ok(ticket.wait_async().await)
    }

    /// Await and reserve the next coalesced exact-scope pass requested by a
    /// watcher, audit, or logical `poll()` caller. This is the source-owner
    /// half of the internal pass driver: returning `None` means attachment
    /// cancellation, while a dropped lease remains pending and wakes a later
    /// driver attempt. Native access and offer work still happen outside the
    /// consumer lock through the lease's bounded access pass.
    pub async fn next_poll_pass(
        &self,
    ) -> Result<Option<ScopedObservationPollLease>, ScopedObservationPollError> {
        let mut waiter = self.shared.host.state.poll.driver_waiter();
        loop {
            match self.shared.host.begin_poll() {
                Ok(Some(lease)) => return Ok(Some(lease)),
                Ok(None) => {}
                Err(ScopedObservationPollError::Closed) => return Ok(None),
                Err(error) => return Err(error),
            }
            match waiter.wait().await {
                ScopedObservationPollDriverResolution::WorkAvailable => {}
                ScopedObservationPollDriverResolution::Closed => return Ok(None),
                ScopedObservationPollDriverResolution::Failed => {
                    return Err(ScopedObservationPollError::ObserverFailed);
                }
                ScopedObservationPollDriverResolution::Pending => {
                    unreachable!("the poll driver waiter filters pending state")
                }
            }
        }
    }

    /// Execute a reserved exact-scope live pass while keeping native access
    /// and decode outside the consumer-drain mutex. Only bounded validation,
    /// offer, and watermark publication enter the attachment lock. The caller
    /// remains the sole owner of `active` for the duration of the pass.
    pub fn execute_epoch_poll_pass(
        &self,
        lease: ScopedObservationPollLease,
        active: &mut ScopedObservationEpochState,
        requests: &[ScopedObservationAppendPassRequest<'_>],
    ) -> Result<Arc<ScopedObservationWatermarkCore>, ScopedObservationPassExecutionError> {
        {
            let drain = self.shared.lock_drain();
            self.shared
                .host
                .validate_epoch_poll_execution(&lease, active, &drain, requests)?;
        }
        {
            let mut drain = self.shared.lock_drain();
            self.shared
                .host
                .offer_epoch_poll_pending(active, &mut drain)?;
        }

        let requests_by_relation = requests
            .iter()
            .map(|request| (request.relation_id, request))
            .collect::<BTreeMap<_, _>>();
        let relation_ids = active.append_objects.keys().cloned().collect::<Vec<_>>();
        for relation_id in relation_ids {
            let request = requests_by_relation
                .get(relation_id.as_str())
                .expect("the exact relation set was prevalidated");
            self.shared
                .host
                .reconcile_epoch_poll_relation(&lease, active, request)?;
            let mut drain = self.shared.lock_drain();
            self.shared
                .host
                .offer_epoch_poll_pending(active, &mut drain)?;
        }

        let drain = self.shared.lock_drain();
        self.shared
            .host
            .complete_epoch_poll(lease, active, &drain)
            .map_err(Into::into)
    }

    /// Transfer one valid active epoch into its automatic long-lived source
    /// owner. Exact owned bindings are checked against both the authorized
    /// scope declaration and the access identity permanently established by
    /// bootstrap before an attachment operation is registered.
    pub fn bind_epoch_source_owner(
        &self,
        active: ScopedObservationEpochState,
        bindings: Vec<ScopedObservationAppendPassBinding>,
        policy: ScopedObservationSourceOwnerRetryPolicy,
    ) -> Result<ScopedObservationAsyncSourceOwner, ScopedObservationSourceOwnerBindFailure> {
        let validation = {
            let drain = self.shared.lock_drain();
            self.shared
                .host
                .validate_epoch_source_owner_binding(&active, &drain, &bindings, policy)
        };
        if let Err(error) = validation {
            return Err(ScopedObservationSourceOwnerBindFailure {
                error,
                active: Box::new(active),
                bindings,
            });
        }
        let operation = match self
            .shared
            .host
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
        {
            Ok(operation) => operation,
            Err(start_error) => {
                let error = match start_error {
                    ScopedObservationOperationStartError::Closing => {
                        ScopedObservationSourceOwnerBindingError::Closed
                    }
                    ScopedObservationOperationStartError::CapacityExhausted => {
                        ScopedObservationSourceOwnerBindingError::OperationCapacityExhausted
                    }
                };
                return Err(ScopedObservationSourceOwnerBindFailure {
                    error,
                    active: Box::new(active),
                    bindings,
                });
            }
        };
        let retry_now = tokio::time::Instant::now();
        let retry_deadlines = active
            .object_errors
            .iter()
            .filter_map(|(relation_id, state)| {
                state
                    .error
                    .retry
                    .retry_delay()
                    .map(|delay| (relation_id.clone(), retry_now + delay))
            })
            .collect();
        Ok(ScopedObservationAsyncSourceOwner {
            handle: self.clone(),
            active,
            bindings,
            policy,
            retry_deadlines,
            operation,
        })
    }

    pub fn prepare_watcher_install(
        &self,
        hint_capacity: usize,
    ) -> Result<ScopedObservationWatcherOrchestrator, ScopedObservationStartupError> {
        self.shared.host.prepare_watcher_install(hint_capacity)
    }

    pub async fn close(&self) -> ScopedObservationCloseState {
        self.request_close().wait_async().await
    }
}

impl ScopedObservationAsyncSourceOwner {
    pub fn scope_epoch(&self) -> u64 {
        self.active.scope_epoch
    }

    /// Run until attachment cancellation, continuity invalidation, or an
    /// attachment-level failure. Delivery queue fullness waits on dequeue-
    /// owned capacity rather than sleeping or invalidating continuity.
    /// Relation-local source/decode failures become explicit coverage plus
    /// bounded object retry controls; they never terminate or delay healthy
    /// sibling relations.
    pub async fn run_until_stopped(self) -> ScopedObservationStoppedSourceOwner {
        self.run_until_stopped_inner(scoped_observation_now_unix_ms)
            .await
    }

    #[cfg(test)]
    pub async fn run_until_stopped_with_clock<C>(
        self,
        observed_at: C,
    ) -> ScopedObservationStoppedSourceOwner
    where
        C: FnMut() -> i64 + Send,
    {
        self.run_until_stopped_inner(observed_at).await
    }

    async fn run_until_stopped_inner<C>(
        mut self,
        mut observed_at: C,
    ) -> ScopedObservationStoppedSourceOwner
    where
        C: FnMut() -> i64 + Send,
    {
        let exit = self.run_loop(&mut observed_at).await;
        let Self {
            handle: _,
            active,
            bindings,
            policy,
            retry_deadlines: _,
            operation,
        } = self;
        // Release the close-barrier obligation before the stopped state is
        // returned and potentially retained by a caller.
        drop(operation);
        ScopedObservationStoppedSourceOwner {
            active,
            bindings,
            policy,
            exit,
        }
    }

    async fn run_loop<C>(&mut self, observed_at: &mut C) -> ScopedObservationSourceOwnerRunExit
    where
        C: FnMut() -> i64 + Send,
    {
        loop {
            let wait_context = match self.attachment_wait_context() {
                Ok(context) => context,
                Err(exit) => return exit,
            };
            let mut automatic_ticket = None;
            let mut lease_result = None;
            let next_deadline = self.retry_deadlines.values().copied().min();
            let attachment_event = if let Some(deadline) = next_deadline {
                tokio::select! {
                    biased;
                    event = self.wait_for_poll_or_attachment_event(
                        &wait_context,
                        &mut lease_result,
                    ) => event,
                    _ = tokio::time::sleep_until(deadline) => {
                        match self.handle.host().request_poll() {
                            Ok(ticket) => {
                                automatic_ticket = Some(ticket);
                                let context = match self.attachment_wait_context() {
                                    Ok(context) => context,
                                    Err(exit) => return exit,
                                };
                                self.wait_for_poll_or_attachment_event(
                                    &context,
                                    &mut lease_result,
                                ).await
                            }
                            Err(ScopedObservationPollError::Closed) => {
                                lease_result = Some(Ok(None));
                                None
                            }
                            Err(error) => {
                                lease_result = Some(Err(error));
                                None
                            }
                        }
                    }
                }
            } else {
                self.wait_for_poll_or_attachment_event(&wait_context, &mut lease_result)
                    .await
            };
            if let Some(state) = attachment_event {
                if state.closed {
                    return ScopedObservationSourceOwnerRunExit::Cancelled;
                }
                continue;
            }
            let lease_result = lease_result
                .expect("a source-owner poll wait without an attachment event has a result");
            // A poll lease and continuity invalidation can become ready in the
            // same scheduler turn. Recheck under the drain lock before any
            // native access so the old owner cannot service a replacement
            // epoch or report a generic pass failure for that race.
            if let Err(exit) = self.attachment_wait_context() {
                return exit;
            }
            let lease = match lease_result {
                Ok(Some(lease)) => lease,
                Ok(None) | Err(ScopedObservationPollError::Closed) => {
                    return ScopedObservationSourceOwnerRunExit::Cancelled;
                }
                Err(error) => {
                    return ScopedObservationSourceOwnerRunExit::Failed(
                        ScopedObservationSourceOwnerRunError::Pass(
                            ScopedObservationPassExecutionError::Poll(error),
                        ),
                    );
                }
            };
            let (capacity_waiter, capacity_generation) = {
                let drain = self.handle.shared.lock_drain();
                let waiter = drain.delivery_capacity_waiter();
                let generation = waiter.snapshot().generation;
                (waiter, generation)
            };
            let attempt_time = observed_at();
            let identity_inputs = self
                .bindings
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
            let origins = self
                .bindings
                .iter()
                .map(|binding| {
                    let mut origin = binding.origin.clone();
                    origin.observed_at = attempt_time;
                    origin
                })
                .collect::<Vec<_>>();
            let requests = self
                .bindings
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

            let result = Self::execute_object_isolated_poll_pass(
                &self.handle,
                &mut self.active,
                &mut self.retry_deadlines,
                self.policy,
                lease,
                &requests,
                attempt_time,
            );
            drop(automatic_ticket);
            // Native access and bounded offers intentionally occur outside
            // the wait. Give an invalidation or terminal failure that raced
            // the synchronous pass precedence over its incidental pass error.
            if let Err(exit) = self.attachment_wait_context() {
                return exit;
            }
            match result {
                Ok(_) => {}
                Err(error) if scoped_source_owner_error_is_cancelled(&error) => {
                    return ScopedObservationSourceOwnerRunExit::Cancelled;
                }
                Err(error) if scoped_source_owner_error_is_delivery_backpressure(&error) => {
                    tokio::select! {
                        biased;
                        _ = self.handle.shared.host.lifecycle.wait_for_close_request_async() => {
                            return ScopedObservationSourceOwnerRunExit::Cancelled;
                        }
                        state = capacity_waiter.wait_after_async(capacity_generation) => {
                            if state.closed {
                                return ScopedObservationSourceOwnerRunExit::Cancelled;
                            }
                        }
                    }
                }
                Err(error) => {
                    return ScopedObservationSourceOwnerRunExit::Failed(
                        ScopedObservationSourceOwnerRunError::Pass(error),
                    );
                }
            }
        }
    }

    fn attachment_wait_context(
        &self,
    ) -> Result<ScopedObservationSourceOwnerWaitContext, ScopedObservationSourceOwnerRunExit> {
        let drain = self.handle.shared.lock_drain();
        if drain.is_closed()
            || self.handle.shared.host.state.closed.load(Ordering::Acquire)
            || self.handle.shared.host.lifecycle.is_closing()
        {
            return Err(ScopedObservationSourceOwnerRunExit::Cancelled);
        }
        let state = drain.delivery_lane().state();
        match state.continuity {
            ScopedObservationContinuity::Valid if state.scope_epoch == self.active.scope_epoch => {
                Ok(ScopedObservationSourceOwnerWaitContext {
                    event_waiter: drain.event_waiter(),
                    offered_through_sequence: state.offered_through_sequence,
                })
            }
            ScopedObservationContinuity::ResyncRequired
            | ScopedObservationContinuity::Resyncing
            | ScopedObservationContinuity::Valid => {
                let control = drain
                    .delivery_lane()
                    .resync_required()
                    .filter(|control| control.invalid_scope_epoch == self.active.scope_epoch);
                Err(ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(
                    ScopedObservationSourceOwnerContinuityInvalidation {
                        owned_scope_epoch: self.active.scope_epoch,
                        observed_scope_epoch: state.scope_epoch,
                        observed_continuity: state.continuity,
                        control,
                    },
                ))
            }
            ScopedObservationContinuity::Failed => {
                Err(ScopedObservationSourceOwnerRunExit::Failed(
                    ScopedObservationSourceOwnerRunError::Pass(
                        ScopedObservationPassExecutionError::Poll(
                            ScopedObservationPollError::ObserverFailed,
                        ),
                    ),
                ))
            }
            ScopedObservationContinuity::Bootstrap => {
                Err(ScopedObservationSourceOwnerRunExit::Failed(
                    ScopedObservationSourceOwnerRunError::Pass(
                        ScopedObservationPassExecutionError::InvalidEpochState,
                    ),
                ))
            }
        }
    }

    async fn wait_for_poll_or_attachment_event(
        &self,
        context: &ScopedObservationSourceOwnerWaitContext,
        poll_result: &mut Option<
            Result<Option<ScopedObservationPollLease>, ScopedObservationPollError>,
        >,
    ) -> Option<ScopedObservationEventWakeState> {
        tokio::select! {
            biased;
            result = self.handle.next_poll_pass() => {
                *poll_result = Some(result);
                None
            }
            state = context
                .event_waiter
                .wait_after_async(context.offered_through_sequence) => {
                Some(state)
            }
        }
    }

    fn execute_object_isolated_poll_pass(
        handle: &ScopedObservationAsyncHandle,
        active: &mut ScopedObservationEpochState,
        retry_deadlines: &mut BTreeMap<String, tokio::time::Instant>,
        policy: ScopedObservationSourceOwnerRetryPolicy,
        lease: ScopedObservationPollLease,
        requests: &[ScopedObservationAppendPassRequest<'_>],
        observed_at: i64,
    ) -> Result<Arc<ScopedObservationWatermarkCore>, ScopedObservationPassExecutionError> {
        {
            let drain = handle.shared.lock_drain();
            handle
                .shared
                .host
                .validate_epoch_poll_execution(&lease, active, &drain, requests)?;
        }
        {
            let mut drain = handle.shared.lock_drain();
            handle
                .shared
                .host
                .offer_epoch_poll_pending(active, &mut drain)?;
        }

        let relation_ids = active.append_objects.keys().cloned().collect::<Vec<_>>();
        for relation_id in &relation_ids {
            Self::offer_pending_object_error(handle, active, relation_id)?;
        }
        let requests_by_relation = requests
            .iter()
            .map(|request| (request.relation_id, request))
            .collect::<BTreeMap<_, _>>();

        for relation_id in relation_ids {
            let retained_error = active.object_errors.get(&relation_id).cloned();
            let retry_not_due = retry_deadlines
                .get(&relation_id)
                .is_some_and(|deadline| *deadline > tokio::time::Instant::now());
            if retained_error
                .as_ref()
                .is_some_and(|state| state.error.retry.is_terminal())
                || retry_not_due
            {
                let object = active
                    .append_objects
                    .get(&relation_id)
                    .expect("the exact relation set was prevalidated");
                active
                    .admission
                    .record_object_error_coverage(
                        object,
                        lease.access_pass().pass_id(),
                        retained_error
                            .as_ref()
                            .expect("only retained object errors own retry deadlines")
                            .error
                            .as_ref(),
                        false,
                    )
                    .map_err(ScopedObservationPassExecutionError::Admission)?;
                continue;
            }

            let request = requests_by_relation
                .get(relation_id.as_str())
                .expect("the exact relation set was prevalidated");
            match handle
                .shared
                .host
                .reconcile_epoch_poll_relation(&lease, active, request)
            {
                Ok(ScopedObservationRelationPollOutcome::Ready) => {
                    active.object_errors.remove(&relation_id);
                    retry_deadlines.remove(&relation_id);
                    let mut drain = handle.shared.lock_drain();
                    handle
                        .shared
                        .host
                        .offer_epoch_poll_pending(active, &mut drain)?;
                }
                Ok(ScopedObservationRelationPollOutcome::RetryTransient) => {
                    Self::record_object_error(
                        handle,
                        active,
                        retry_deadlines,
                        policy,
                        &lease,
                        &relation_id,
                        ScopedObjectFailureClassification::Retryable(
                            ScopedSourceObjectFailureCode::SourceRetryTransient,
                        ),
                        observed_at,
                    )?;
                }
                Err(error) => {
                    let Some(classification) = scoped_object_failure_classification(&error) else {
                        return Err(error);
                    };
                    Self::record_object_error(
                        handle,
                        active,
                        retry_deadlines,
                        policy,
                        &lease,
                        &relation_id,
                        classification,
                        observed_at,
                    )?;
                }
            }
        }

        let drain = handle.shared.lock_drain();
        handle
            .shared
            .host
            .complete_epoch_poll(lease, active, &drain)
            .map_err(Into::into)
    }

    fn offer_pending_object_error(
        handle: &ScopedObservationAsyncHandle,
        active: &mut ScopedObservationEpochState,
        relation_id: &str,
    ) -> Result<(), ScopedObservationPassExecutionError> {
        let Some(state) = active.object_errors.get(relation_id) else {
            return Ok(());
        };
        if state.control_offered {
            return Ok(());
        }
        let error = Arc::clone(&state.error);
        let observed_at = state.observed_at;
        let object = active
            .append_objects
            .get(relation_id)
            .expect("a retained object error belongs to the active exact relation set");
        let mut drain = handle.shared.lock_drain();
        handle
            .shared
            .host
            .offer_consumer_object_error(&mut drain, object, error, observed_at)?;
        active
            .object_errors
            .get_mut(relation_id)
            .expect("the pending object error remains owned by the active epoch")
            .control_offered = true;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_object_error(
        handle: &ScopedObservationAsyncHandle,
        active: &mut ScopedObservationEpochState,
        retry_deadlines: &mut BTreeMap<String, tokio::time::Instant>,
        policy: ScopedObservationSourceOwnerRetryPolicy,
        lease: &ScopedObservationPollLease,
        relation_id: &str,
        classification: ScopedObjectFailureClassification,
        observed_at: i64,
    ) -> Result<(), ScopedObservationPassExecutionError> {
        let prior_attempts = active
            .object_errors
            .get(relation_id)
            .map_or(0, |state| state.error.retry.failed_attempts());
        let failed_attempts = prior_attempts
            .checked_add(1)
            .expect("bounded object retry attempts cannot overflow");
        let (failure_code, retry) = match classification {
            ScopedObjectFailureClassification::Retryable(failure_code)
                if failed_attempts < policy.max_transient_attempts =>
            {
                let retry_delay = policy.retry_delay(failed_attempts - 1);
                let retry_after_ms = u64::try_from(retry_delay.as_millis())
                    .expect("the bounded retry duration fits u64 milliseconds");
                retry_deadlines.insert(
                    relation_id.to_string(),
                    tokio::time::Instant::now() + retry_delay,
                );
                (
                    failure_code,
                    ScopedSourceObjectRetryState::RetryScheduled {
                        failed_attempts,
                        max_attempts: policy.max_transient_attempts,
                        retry_after_ms,
                    },
                )
            }
            ScopedObjectFailureClassification::Retryable(failure_code) => {
                retry_deadlines.remove(relation_id);
                (
                    failure_code,
                    ScopedSourceObjectRetryState::RetryExhausted {
                        failed_attempts,
                        max_attempts: policy.max_transient_attempts,
                    },
                )
            }
            ScopedObjectFailureClassification::Terminal(failure_code) => {
                retry_deadlines.remove(relation_id);
                (
                    failure_code,
                    ScopedSourceObjectRetryState::NotRetryable { failed_attempts },
                )
            }
        };
        let object = active
            .append_objects
            .get(relation_id)
            .expect("the exact relation set was prevalidated");
        let error = Arc::new(ScopedSourceObjectError {
            error_contract_version: SCOPED_SOURCE_OBJECT_ERROR_CONTRACT_VERSION,
            relation_id: Arc::from(relation_id),
            source: object.source.clone(),
            scope_epoch: active.scope_epoch,
            failure_code,
            provenance: object.object_error_provenance()?,
            retry,
        });
        debug_assert!(error.validate());
        active.object_errors.insert(
            relation_id.to_string(),
            ScopedSourceObjectErrorRuntime {
                error: Arc::clone(&error),
                observed_at,
                control_offered: false,
            },
        );
        Self::offer_pending_object_error(handle, active, relation_id)?;
        let object = active
            .append_objects
            .get(relation_id)
            .expect("the exact relation set was prevalidated");
        active
            .admission
            .record_object_error_coverage(object, lease.access_pass().pass_id(), &error, true)
            .map_err(ScopedObservationPassExecutionError::Admission)
    }
}

/// Internal executor-friendly lifecycle facade for one scoped attachment.
/// It owns the sole consumer drain from before bootstrap, exposes one
/// non-cloneable ordered event iterator, and keeps engine readiness distinct
/// from consumer application acknowledgement. The containing module remains
/// crate-private until RFC 012D scope/envelope coverage is complete.
pub struct ScopedObservationAsyncRuntime {
    shared: Arc<ScopedObservationAsyncRuntimeShared>,
}

impl ScopedObservationAsyncRuntime {
    pub fn open(
        host: ScopedObservationAccessHost,
        limits: ScopedObservationDeliveryLimits,
    ) -> Result<Self, ScopedObservationOpenDrainError> {
        let drain = host.open_consumer_drain(limits)?;
        Ok(Self {
            shared: Arc::new(ScopedObservationAsyncRuntimeShared {
                host,
                drain: Mutex::new(drain),
            }),
        })
    }

    /// Cloneable control/producer half. Keeping readiness and poll on this
    /// handle lets them run concurrently with `next_event(&mut self)` while
    /// the runtime value itself remains the one non-cloneable event owner.
    pub fn handle(&self) -> ScopedObservationAsyncHandle {
        ScopedObservationAsyncHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Yield the next ordered envelope, or `None` after attachment close.
    /// Queue inspection and wake-state capture share the producer lock; the
    /// actual await never holds it, preventing both lost wakeups and producer
    /// starvation.
    pub async fn next_event(
        &mut self,
    ) -> Result<Option<ScopedObservationYieldedEnvelope>, ScopedObservationDrainError> {
        loop {
            let (waiter, offered_through_sequence) = {
                let mut drain = self.shared.lock_drain();
                match drain.next() {
                    Ok(Some(yielded)) => return Ok(Some(yielded)),
                    Ok(None) => {}
                    Err(ScopedObservationDrainError::Closed) => return Ok(None),
                    Err(error) => return Err(error),
                }
                let waiter = drain.event_waiter();
                let wake = waiter.snapshot();
                if wake.closed {
                    drain.close();
                    return Ok(None);
                }
                (waiter, wake.offered_through_sequence)
            };
            if waiter
                .wait_after_async(offered_through_sequence)
                .await
                .closed
            {
                self.shared.lock_drain().close();
                return Ok(None);
            }
        }
    }

    pub fn acknowledge_applied(
        &mut self,
        receipt: &ScopedObservationApplicationReceipt,
    ) -> Result<ScopedObservationAppliedState, ScopedObservationApplicationError> {
        self.shared.lock_drain().acknowledge_applied(receipt)
    }

    pub fn applied_state(&self) -> ScopedObservationAppliedState {
        self.shared.lock_drain().state()
    }

    pub fn request_close(&self) -> ScopedObservationCloseBarrier {
        self.shared.request_close()
    }

    /// Request cancellation, close the sole drain, and await watcher/native
    /// acknowledgement without holding the consumer lock.
    pub async fn close(&self) -> ScopedObservationCloseState {
        self.request_close().wait_async().await
    }
}

impl Drop for ScopedObservationAsyncRuntime {
    fn drop(&mut self) {
        let _ = self.request_close();
    }
}

pub type ScopedObservationNativeWatchCallback =
    Box<dyn FnMut(notify::Result<Event>) + Send + 'static>;

/// Object-safe backend seam used by the concrete `notify` owner and by
/// deterministic watcher-before-scan tests. The containing module remains
/// crate-private, so portable consumers cannot inject a watcher backend.
pub trait ScopedObservationNativeWatchBackend: Send {
    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> Result<(), ()>;
}

impl ScopedObservationNativeWatchBackend for RecommendedWatcher {
    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> Result<(), ()> {
        Watcher::watch(self, path, mode).map_err(|_| ())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopedObservationNativeWatcherState {
    /// Successful native backend installation generation. The initial
    /// installation is generation one; a failed replacement never advances
    /// it.
    pub backend_generation: u64,
    pub generation: u64,
    pub backend_failed: bool,
    pub routing_failed: bool,
    pub reinstalling: bool,
    pub closed: bool,
}

#[derive(Debug)]
struct ScopedObservationNativeWatcherCompletion {
    state: Mutex<ScopedObservationNativeWatcherState>,
    async_changed: tokio::sync::watch::Sender<ScopedObservationNativeWatcherState>,
}

impl Default for ScopedObservationNativeWatcherCompletion {
    fn default() -> Self {
        let state = ScopedObservationNativeWatcherState::default();
        let (async_changed, _) = tokio::sync::watch::channel(state);
        Self {
            state: Mutex::new(state),
            async_changed,
        }
    }
}

impl ScopedObservationNativeWatcherCompletion {
    fn snapshot(&self) -> ScopedObservationNativeWatcherState {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish(&self, backend_failed: bool, routing_failed: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.backend_failed |= backend_failed;
        state.routing_failed |= routing_failed;
        advance_scoped_native_watcher_generation(&mut state);
        self.async_changed.send_replace(*state);
    }

    fn mark_initial_backend_installed(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        debug_assert_eq!(state.backend_generation, 0);
        state.backend_generation = 1;
        self.async_changed.send_replace(*state);
    }

    fn begin_reinstall(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.backend_failed = false;
        state.reinstalling = true;
        advance_scoped_native_watcher_generation(&mut state);
        self.async_changed.send_replace(*state);
    }

    fn finish_reinstall_failure(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.backend_failed = true;
        state.reinstalling = false;
        advance_scoped_native_watcher_generation(&mut state);
        self.async_changed.send_replace(*state);
    }

    fn finish_reinstall_success(&self) -> Result<(), ()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(backend_generation) = state.backend_generation.checked_add(1) else {
            state.routing_failed = true;
            state.reinstalling = false;
            advance_scoped_native_watcher_generation(&mut state);
            self.async_changed.send_replace(*state);
            return Err(());
        };
        state.backend_generation = backend_generation;
        state.backend_failed = false;
        state.reinstalling = false;
        advance_scoped_native_watcher_generation(&mut state);
        self.async_changed.send_replace(*state);
        Ok(())
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.closed {
            state.closed = true;
            self.async_changed.send_replace(*state);
        }
    }

    async fn wait_after(&self, generation: u64) -> ScopedObservationNativeWatcherState {
        let mut changed = self.async_changed.subscribe();
        loop {
            let state = *changed.borrow_and_update();
            if state.closed
                || state.backend_failed
                || state.routing_failed
                || state.generation > generation
            {
                return state;
            }
            changed
                .changed()
                .await
                .expect("scoped native watcher retains its async notification sender");
        }
    }
}

fn advance_scoped_native_watcher_generation(state: &mut ScopedObservationNativeWatcherState) {
    match state.generation.checked_add(1) {
        Some(generation) => state.generation = generation,
        None => state.routing_failed = true,
    }
}

#[derive(Debug, Clone)]
pub struct ScopedObservationNativeWatcherWaiter {
    completion: Arc<ScopedObservationNativeWatcherCompletion>,
}

impl ScopedObservationNativeWatcherWaiter {
    pub fn state(&self) -> ScopedObservationNativeWatcherState {
        self.completion.snapshot()
    }

    pub async fn wait_after(&self, generation: u64) -> ScopedObservationNativeWatcherState {
        self.completion.wait_after(generation).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationNativeWatcherError {
    #[error("scoped watcher startup failed: {0}")]
    Startup(#[from] ScopedObservationStartupError),
    #[error("scoped known-object relation {relation_id:?} has no existing watch root")]
    NoWatchableRoot { relation_id: String },
    #[error("scoped native watcher backend could not be created")]
    BackendUnavailable,
    #[error("scoped native watcher could not register authorized anchor {anchor_index}")]
    AnchorRegistrationFailed { anchor_index: usize },
    #[error("scoped native watcher callback failed during installation")]
    CallbackFailedDuringInstall,
    #[error("scoped native watcher callback failed during backend replacement")]
    CallbackFailedDuringReinstall,
    #[error("scoped native watcher routing is terminally failed")]
    RoutingFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationNativeWatcherRecoveryPolicyError {
    #[error("scoped watcher audit interval is outside the supported bound")]
    AuditInterval,
    #[error("scoped watcher retry interval is outside the supported bound")]
    RetryInterval,
    #[error("scoped watcher replacement attempt limit is outside the supported bound")]
    AttemptLimit,
}

/// Bounded runtime policy for watcher audits and backend replacement. These
/// are internal correctness ceilings, not promoted RFC 012D performance gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationNativeWatcherRecoveryPolicy {
    audit_interval: Duration,
    initial_retry_delay: Duration,
    max_retry_delay: Duration,
    max_reinstall_attempts: u32,
}

impl ScopedObservationNativeWatcherRecoveryPolicy {
    const MAX_AUDIT_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
    const MAX_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);
    const MAX_REINSTALL_ATTEMPTS: u32 = 32;

    pub fn new(
        audit_interval: Duration,
        initial_retry_delay: Duration,
        max_retry_delay: Duration,
        max_reinstall_attempts: u32,
    ) -> Result<Self, ScopedObservationNativeWatcherRecoveryPolicyError> {
        if audit_interval.is_zero() || audit_interval > Self::MAX_AUDIT_INTERVAL {
            return Err(ScopedObservationNativeWatcherRecoveryPolicyError::AuditInterval);
        }
        if initial_retry_delay.is_zero()
            || initial_retry_delay > max_retry_delay
            || max_retry_delay > Self::MAX_RETRY_DELAY
        {
            return Err(ScopedObservationNativeWatcherRecoveryPolicyError::RetryInterval);
        }
        if max_reinstall_attempts == 0 || max_reinstall_attempts > Self::MAX_REINSTALL_ATTEMPTS {
            return Err(ScopedObservationNativeWatcherRecoveryPolicyError::AttemptLimit);
        }
        Ok(Self {
            audit_interval,
            initial_retry_delay,
            max_retry_delay,
            max_reinstall_attempts,
        })
    }

    pub fn audit_interval(self) -> Duration {
        self.audit_interval
    }

    pub fn initial_retry_delay(self) -> Duration {
        self.initial_retry_delay
    }

    pub fn max_retry_delay(self) -> Duration {
        self.max_retry_delay
    }

    pub fn max_reinstall_attempts(self) -> u32 {
        self.max_reinstall_attempts
    }

    fn retry_delay(self, completed_attempts: u32) -> Duration {
        let exponent = completed_attempts.min(31);
        self.initial_retry_delay
            .saturating_mul(1_u32 << exponent)
            .min(self.max_retry_delay)
    }
}

impl Default for ScopedObservationNativeWatcherRecoveryPolicy {
    fn default() -> Self {
        Self {
            audit_interval: Duration::from_secs(5 * 60),
            initial_retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
            max_reinstall_attempts: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedObservationNativeWatcherRunExit {
    Cancelled,
    Failed(Arc<ScopedObserverFailure>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedObservationNativeWatchAnchor {
    path: PathBuf,
    recursive: bool,
}

#[derive(Debug, Clone)]
struct ScopedObservationNativeWatchRoute {
    relation_id: Vec<u8>,
    target_aliases: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct ScopedObservationNativeWatchPlan {
    instance_scope: Vec<u8>,
    anchors: Vec<ScopedObservationNativeWatchAnchor>,
    routes: Vec<ScopedObservationNativeWatchRoute>,
}

impl ScopedObservationNativeWatchPlan {
    fn from_host(
        host: &ScopedObservationAccessHost,
    ) -> Result<Self, ScopedObservationNativeWatcherError> {
        let mut anchors = Vec::with_capacity(host.known_objects.len());
        let mut routes = Vec::with_capacity(host.known_objects.len());
        for grant in host.known_objects.values() {
            let canonical_root = grant.root.canonicalize().map_err(|_| {
                ScopedObservationNativeWatcherError::NoWatchableRoot {
                    relation_id: grant.relation_id.clone(),
                }
            })?;
            if !canonical_root.is_dir() {
                return Err(ScopedObservationNativeWatcherError::NoWatchableRoot {
                    relation_id: grant.relation_id.clone(),
                });
            }
            if canonical_root.parent().is_none() {
                return Err(ScopedObservationNativeWatcherError::NoWatchableRoot {
                    relation_id: grant.relation_id.clone(),
                });
            }
            let logical_target = grant.root.join(&grant.relative_path);
            let canonical_target = canonical_root.join(&grant.relative_path);
            let logical_parent = logical_target.parent().ok_or_else(|| {
                ScopedObservationNativeWatcherError::NoWatchableRoot {
                    relation_id: grant.relation_id.clone(),
                }
            })?;
            let mut candidate = logical_parent;
            let (anchor, recursive) = loop {
                if candidate.is_dir() {
                    let anchor = candidate.canonicalize().map_err(|_| {
                        ScopedObservationNativeWatcherError::NoWatchableRoot {
                            relation_id: grant.relation_id.clone(),
                        }
                    })?;
                    let canonical_parent = canonical_target.parent().ok_or_else(|| {
                        ScopedObservationNativeWatcherError::NoWatchableRoot {
                            relation_id: grant.relation_id.clone(),
                        }
                    })?;
                    if !anchor.starts_with(&canonical_root) {
                        return Err(ScopedObservationNativeWatcherError::NoWatchableRoot {
                            relation_id: grant.relation_id.clone(),
                        });
                    }
                    let recursive = anchor != canonical_parent;
                    break (anchor, recursive);
                }
                if candidate == grant.root {
                    return Err(ScopedObservationNativeWatcherError::NoWatchableRoot {
                        relation_id: grant.relation_id.clone(),
                    });
                }
                candidate = candidate.parent().ok_or_else(|| {
                    ScopedObservationNativeWatcherError::NoWatchableRoot {
                        relation_id: grant.relation_id.clone(),
                    }
                })?;
                if !candidate.starts_with(&grant.root) {
                    return Err(ScopedObservationNativeWatcherError::NoWatchableRoot {
                        relation_id: grant.relation_id.clone(),
                    });
                }
            };
            anchors.push(ScopedObservationNativeWatchAnchor {
                path: anchor,
                recursive,
            });
            let mut target_aliases = vec![logical_target, canonical_target];
            target_aliases.sort();
            target_aliases.dedup();
            routes.push(ScopedObservationNativeWatchRoute {
                relation_id: grant.relation_id.as_bytes().to_vec(),
                target_aliases,
            });
        }
        consolidate_scoped_watch_anchors(&mut anchors);
        Ok(Self {
            instance_scope: host.root_identity.source_instance_key.as_bytes().to_vec(),
            anchors,
            routes,
        })
    }

    fn route(&self, event: notify::Result<Event>) -> ScopedObservationNativeWatchIngress {
        let event = match event {
            Ok(event) => event,
            Err(_) => {
                return ScopedObservationNativeWatchIngress {
                    hints: vec![DirtyHint {
                        scope: DirtyScope::Instance(self.instance_scope.clone()),
                        reason: DirtyReason::BackendError,
                    }],
                    backend_failed: true,
                };
            }
        };
        if event.need_rescan() {
            return ScopedObservationNativeWatchIngress {
                hints: vec![DirtyHint {
                    scope: DirtyScope::Instance(self.instance_scope.clone()),
                    reason: DirtyReason::WatcherOverflow,
                }],
                backend_failed: false,
            };
        }
        if event.kind.is_access() {
            return ScopedObservationNativeWatchIngress::default();
        }
        if event.paths.is_empty() {
            return ScopedObservationNativeWatchIngress {
                hints: vec![DirtyHint {
                    scope: DirtyScope::Instance(self.instance_scope.clone()),
                    reason: DirtyReason::NativeEvent,
                }],
                backend_failed: false,
            };
        }

        let membership_change = matches!(
            event.kind,
            EventKind::Create(_)
                | EventKind::Remove(_)
                | EventKind::Modify(notify::event::ModifyKind::Name(_))
        );
        let mut relations = std::collections::BTreeSet::new();
        let mut instance_dirty = false;
        for path in event.paths {
            if !path.is_absolute() {
                instance_dirty = true;
                break;
            }
            let mut aliases = vec![path.clone()];
            if let Ok(canonical) = path.canonicalize() {
                aliases.push(canonical);
            }
            aliases.sort();
            aliases.dedup();
            for route in &self.routes {
                if aliases.iter().any(|path| {
                    route
                        .target_aliases
                        .iter()
                        .any(|target| path == target || path.starts_with(target))
                }) {
                    relations.insert(route.relation_id.clone());
                } else if membership_change
                    && aliases.iter().any(|path| {
                        route
                            .target_aliases
                            .iter()
                            .any(|target| target.starts_with(path))
                    })
                {
                    instance_dirty = true;
                }
            }
        }
        let hints = if instance_dirty {
            vec![DirtyHint {
                scope: DirtyScope::Instance(self.instance_scope.clone()),
                reason: DirtyReason::NativeEvent,
            }]
        } else {
            relations
                .into_iter()
                .map(|relation_id| DirtyHint {
                    scope: DirtyScope::Object(relation_id),
                    reason: DirtyReason::NativeEvent,
                })
                .collect()
        };
        ScopedObservationNativeWatchIngress {
            hints,
            backend_failed: false,
        }
    }
}

#[derive(Debug, Default)]
struct ScopedObservationNativeWatchIngress {
    hints: Vec<DirtyHint>,
    backend_failed: bool,
}

fn consolidate_scoped_watch_anchors(anchors: &mut Vec<ScopedObservationNativeWatchAnchor>) {
    anchors.sort_by(|left, right| {
        left.path
            .components()
            .count()
            .cmp(&right.path.components().count())
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| right.recursive.cmp(&left.recursive))
    });
    let mut consolidated: Vec<ScopedObservationNativeWatchAnchor> =
        Vec::with_capacity(anchors.len());
    for anchor in anchors.drain(..) {
        if let Some(existing) = consolidated
            .iter_mut()
            .find(|existing| existing.path == anchor.path)
        {
            existing.recursive |= anchor.recursive;
            continue;
        }
        if consolidated
            .iter()
            .any(|existing| existing.recursive && anchor.path.starts_with(&existing.path))
        {
            continue;
        }
        consolidated.push(anchor);
    }
    *anchors = consolidated;
}

fn scoped_native_watch_callback(
    handle: ScopedObservationAsyncHandle,
    coordinator: Arc<ScopedObservationWatcherOrchestrator>,
    plan: Arc<ScopedObservationNativeWatchPlan>,
    completion: Arc<ScopedObservationNativeWatcherCompletion>,
) -> ScopedObservationNativeWatchCallback {
    Box::new(move |event| {
        let ingress = plan.route(event);
        if ingress.hints.is_empty() {
            return;
        }
        let mut routing_failed = false;
        for hint in ingress.hints {
            match coordinator.record_hint(handle.host(), hint) {
                Ok(ScopedObservationWatcherHintAction::Buffered(_))
                | Ok(ScopedObservationWatcherHintAction::PollRequested { .. }) => {}
                Err(ScopedObservationStartupError::Closed) => return,
                Err(_) => routing_failed = true,
            }
        }
        completion.publish(ingress.backend_failed, routing_failed);
    })
}

fn install_scoped_native_watch_backend<F>(
    plan: &ScopedObservationNativeWatchPlan,
    callback: ScopedObservationNativeWatchCallback,
    factory: F,
) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ScopedObservationNativeWatcherError>
where
    F: FnOnce(
        ScopedObservationNativeWatchCallback,
    ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>,
{
    let mut backend =
        factory(callback).map_err(|_| ScopedObservationNativeWatcherError::BackendUnavailable)?;
    for (anchor_index, anchor) in plan.anchors.iter().enumerate() {
        let mode = if anchor.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        backend.watch(&anchor.path, mode).map_err(|_| {
            ScopedObservationNativeWatcherError::AnchorRegistrationFailed { anchor_index }
        })?;
    }
    Ok(backend)
}

/// Concrete native watcher owner. Dropping the backend stops callbacks before
/// the coordinator registration is released, so the attachment close barrier
/// cannot complete while a native callback may still run.
pub struct ScopedObservationNativeWatcher {
    backend: Option<Box<dyn ScopedObservationNativeWatchBackend>>,
    handle: ScopedObservationAsyncHandle,
    coordinator: Arc<ScopedObservationWatcherOrchestrator>,
    plan: Arc<ScopedObservationNativeWatchPlan>,
    completion: Arc<ScopedObservationNativeWatcherCompletion>,
    watch_anchor_count: usize,
    terminal_failure_delivered: bool,
}

impl std::fmt::Debug for ScopedObservationNativeWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationNativeWatcher")
            .field("phase", &self.coordinator.phase())
            .field("watch_anchor_count", &self.watch_anchor_count)
            .field("state", &self.completion.snapshot())
            .finish_non_exhaustive()
    }
}

impl ScopedObservationNativeWatcher {
    pub fn coordinator(&self) -> &ScopedObservationWatcherOrchestrator {
        &self.coordinator
    }

    pub fn watch_anchor_count(&self) -> usize {
        self.watch_anchor_count
    }

    pub fn state(&self) -> ScopedObservationNativeWatcherState {
        self.completion.snapshot()
    }

    pub fn waiter(&self) -> ScopedObservationNativeWatcherWaiter {
        ScopedObservationNativeWatcherWaiter {
            completion: Arc::clone(&self.completion),
        }
    }

    /// Schedule a bounded full-instance reconciliation without fabricating a
    /// native event. Before bootstrap this joins the startup hint set; once
    /// live it creates an ordinary request-local poll ticket for the pass
    /// driver.
    pub fn request_audit(
        &self,
    ) -> Result<ScopedObservationWatcherHintAction, ScopedObservationNativeWatcherError> {
        self.request_instance_reconcile(DirtyReason::Recovery)
    }

    pub fn fail_observer(
        &mut self,
        reason: ScopedObserverFailureReason,
        observed_at: i64,
    ) -> Result<Arc<ScopedObserverFailure>, ScopedContinuityError> {
        let failure = self.handle.fail_observer(reason, observed_at)?;
        self.terminal_failure_delivered = true;
        Ok(failure)
    }

    /// Replace a failed native backend while retaining this attachment's one
    /// watcher registration and callback routing authority. A successful
    /// replacement always schedules a full-instance reconciliation because
    /// notifications may have been lost while the backend was unavailable.
    pub fn reinstall_native_backend(
        &mut self,
    ) -> Result<ScopedObservationWatcherHintAction, ScopedObservationNativeWatcherError> {
        self.reinstall_native_backend_with_factory_inner(|callback| {
            let watcher = notify::recommended_watcher(callback).map_err(|_| ())?;
            Ok(Box::new(watcher))
        })
    }

    fn request_instance_reconcile(
        &self,
        reason: DirtyReason,
    ) -> Result<ScopedObservationWatcherHintAction, ScopedObservationNativeWatcherError> {
        let hint = DirtyHint {
            scope: DirtyScope::Instance(self.plan.instance_scope.clone()),
            reason,
        };
        match self.coordinator.record_hint(self.handle.host(), hint) {
            Ok(action) => {
                self.completion.publish(false, false);
                Ok(action)
            }
            Err(error) => {
                if !matches!(error, ScopedObservationStartupError::Closed) {
                    self.completion.publish(false, true);
                }
                Err(error.into())
            }
        }
    }

    fn reinstall_native_backend_with_factory_inner<F>(
        &mut self,
        factory: F,
    ) -> Result<ScopedObservationWatcherHintAction, ScopedObservationNativeWatcherError>
    where
        F: FnOnce(
            ScopedObservationNativeWatchCallback,
        ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>,
    {
        if self.coordinator.cancellation_requested() {
            return Err(ScopedObservationStartupError::Closed.into());
        }
        if self.completion.snapshot().routing_failed {
            return Err(ScopedObservationNativeWatcherError::RoutingFailed);
        }

        self.completion.begin_reinstall();
        drop(self.backend.take());
        let callback = scoped_native_watch_callback(
            self.handle.clone(),
            Arc::clone(&self.coordinator),
            Arc::clone(&self.plan),
            Arc::clone(&self.completion),
        );
        let backend = match install_scoped_native_watch_backend(&self.plan, callback, factory) {
            Ok(backend) => backend,
            Err(error) => {
                self.completion.finish_reinstall_failure();
                return Err(error);
            }
        };
        let callback_state = self.completion.snapshot();
        if callback_state.backend_failed || callback_state.routing_failed {
            self.completion.finish_reinstall_failure();
            return Err(ScopedObservationNativeWatcherError::CallbackFailedDuringReinstall);
        }

        self.backend = Some(backend);
        self.completion
            .finish_reinstall_success()
            .map_err(|()| ScopedObservationNativeWatcherError::RoutingFailed)?;
        self.request_instance_reconcile(DirtyReason::Recovery)
    }

    #[cfg(test)]
    pub fn reinstall_native_backend_with_factory<F>(
        &mut self,
        factory: F,
    ) -> Result<ScopedObservationWatcherHintAction, ScopedObservationNativeWatcherError>
    where
        F: FnOnce(
            ScopedObservationNativeWatchCallback,
        ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>,
    {
        self.reinstall_native_backend_with_factory_inner(factory)
    }

    /// Own the watcher until cancellation or terminal recovery exhaustion.
    /// Audits only schedule bounded whole-scope passes; the attachment's pass
    /// driver remains the sole source reader and watermark publisher.
    pub async fn run_with_recovery(
        self,
        policy: ScopedObservationNativeWatcherRecoveryPolicy,
    ) -> Result<ScopedObservationNativeWatcherRunExit, ScopedContinuityError> {
        self.run_with_recovery_inner(
            policy,
            |callback| {
                let watcher = notify::recommended_watcher(callback).map_err(|_| ())?;
                Ok(Box::new(watcher))
            },
            scoped_observation_now_unix_ms,
        )
        .await
    }

    async fn run_with_recovery_inner<F, C>(
        mut self,
        policy: ScopedObservationNativeWatcherRecoveryPolicy,
        mut factory: F,
        mut observed_at: C,
    ) -> Result<ScopedObservationNativeWatcherRunExit, ScopedContinuityError>
    where
        F: FnMut(
                ScopedObservationNativeWatchCallback,
            ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>
            + Send,
        C: FnMut() -> i64 + Send,
    {
        self.run_with_recovery_loop(policy, &mut factory, &mut observed_at)
            .await
    }

    async fn run_with_recovery_loop<F, C>(
        &mut self,
        policy: ScopedObservationNativeWatcherRecoveryPolicy,
        factory: &mut F,
        observed_at: &mut C,
    ) -> Result<ScopedObservationNativeWatcherRunExit, ScopedContinuityError>
    where
        F: FnMut(
                ScopedObservationNativeWatchCallback,
            ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>
            + Send,
        C: FnMut() -> i64 + Send,
    {
        let mut audit = tokio::time::interval(policy.audit_interval());
        audit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `interval` ticks immediately once; consume that setup tick so audits
        // happen after the configured quiet period rather than at bootstrap.
        audit.tick().await;
        let mut observed_generation = self.state().generation;

        loop {
            let state = self.state();
            if state.routing_failed {
                return self.finish_terminal_failure(
                    ScopedObserverFailureReason::NativeWatcherRoutingFailed,
                    observed_at(),
                );
            }
            if state.backend_failed {
                let mut completed_attempts = 0;
                loop {
                    if completed_attempts == policy.max_reinstall_attempts() {
                        return self.finish_terminal_failure(
                            ScopedObserverFailureReason::NativeWatcherRecoveryExhausted,
                            observed_at(),
                        );
                    }
                    let delay = policy.retry_delay(completed_attempts);
                    tokio::select! {
                        biased;
                        _ = self.coordinator.wait_for_cancellation_async() => {
                            return Ok(ScopedObservationNativeWatcherRunExit::Cancelled);
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                    completed_attempts += 1;
                    match self.reinstall_native_backend_with_factory_inner(&mut *factory) {
                        Ok(_) => {
                            observed_generation = self.state().generation;
                            break;
                        }
                        Err(ScopedObservationNativeWatcherError::Startup(
                            ScopedObservationStartupError::Closed,
                        )) => {
                            return Ok(ScopedObservationNativeWatcherRunExit::Cancelled);
                        }
                        Err(_) if self.state().routing_failed => {
                            return self.finish_terminal_failure(
                                ScopedObserverFailureReason::NativeWatcherRoutingFailed,
                                observed_at(),
                            );
                        }
                        Err(_) => {}
                    }
                }
                continue;
            }

            let waiter = self.waiter();
            tokio::select! {
                biased;
                _ = self.coordinator.wait_for_cancellation_async() => {
                    return Ok(ScopedObservationNativeWatcherRunExit::Cancelled);
                }
                changed = waiter.wait_after(observed_generation) => {
                    observed_generation = changed.generation;
                }
                _ = audit.tick() => {
                    match self.request_audit() {
                        Ok(_) => observed_generation = self.state().generation,
                        Err(ScopedObservationNativeWatcherError::Startup(
                            ScopedObservationStartupError::Closed,
                        )) => {
                            return Ok(ScopedObservationNativeWatcherRunExit::Cancelled);
                        }
                        Err(_) => {
                            return self.finish_terminal_failure(
                                ScopedObserverFailureReason::NativeWatcherRoutingFailed,
                                observed_at(),
                            );
                        }
                    }
                }
            }
        }
    }

    fn finish_terminal_failure(
        &mut self,
        reason: ScopedObserverFailureReason,
        observed_at: i64,
    ) -> Result<ScopedObservationNativeWatcherRunExit, ScopedContinuityError> {
        match self.fail_observer(reason, observed_at) {
            Ok(failure) => Ok(ScopedObservationNativeWatcherRunExit::Failed(failure)),
            Err(ScopedContinuityError::Closed) => {
                Ok(ScopedObservationNativeWatcherRunExit::Cancelled)
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    pub async fn run_with_recovery_with_factory_and_clock<F, C>(
        self,
        policy: ScopedObservationNativeWatcherRecoveryPolicy,
        factory: F,
        observed_at: C,
    ) -> Result<ScopedObservationNativeWatcherRunExit, ScopedContinuityError>
    where
        F: FnMut(
                ScopedObservationNativeWatchCallback,
            ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>
            + Send,
        C: FnMut() -> i64 + Send,
    {
        self.run_with_recovery_inner(policy, factory, observed_at)
            .await
    }

    /// Own the native backend until attachment cancellation, then stop the
    /// backend and release the coordinator's watcher registration on return.
    pub async fn run_until_cancelled(self) {
        self.coordinator.wait_for_cancellation_async().await;
    }
}

fn scoped_observation_now_unix_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    i64::try_from(millis).unwrap_or(i64::MAX)
}

impl Drop for ScopedObservationNativeWatcher {
    fn drop(&mut self) {
        drop(self.backend.take());
        if !self.coordinator.cancellation_requested() && !self.terminal_failure_delivered {
            self.handle.shared.request_close_unless_observer_failed();
        }
        self.completion.close();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationAsyncOwnerPairBindingError {
    #[error("scoped watcher and source owners belong to another attachment")]
    ForeignAttachment,
    #[error("scoped watcher owner has not reached the live phase")]
    WatcherNotLive,
    #[error("scoped watcher owner is terminally failed")]
    WatcherFailed,
    #[error("scoped source owner does not own the attachment's current valid epoch")]
    SourceNotLive,
}

/// Failed watcher/source pairing returns both non-cloneable owners intact, so
/// a caller cannot lose native callback or source epoch authority while
/// correcting lifecycle ordering.
pub struct ScopedObservationAsyncOwnerPairBindFailure {
    error: ScopedObservationAsyncOwnerPairBindingError,
    watcher: Box<ScopedObservationNativeWatcher>,
    source: Box<ScopedObservationAsyncSourceOwner>,
    watcher_policy: ScopedObservationNativeWatcherRecoveryPolicy,
}

impl ScopedObservationAsyncOwnerPairBindFailure {
    pub fn error(&self) -> ScopedObservationAsyncOwnerPairBindingError {
        self.error
    }

    pub fn into_parts(
        self,
    ) -> (
        ScopedObservationAsyncOwnerPairBindingError,
        ScopedObservationNativeWatcher,
        ScopedObservationAsyncSourceOwner,
        ScopedObservationNativeWatcherRecoveryPolicy,
    ) {
        (self.error, *self.watcher, *self.source, self.watcher_policy)
    }
}

impl std::fmt::Debug for ScopedObservationAsyncOwnerPairBindFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationAsyncOwnerPairBindFailure")
            .field("error", &self.error)
            .field("watcher_phase", &self.watcher.coordinator.phase())
            .field("watch_anchor_count", &self.watcher.watch_anchor_count)
            .field("source_scope_epoch", &self.source.active.scope_epoch)
            .field("watcher_policy", &self.watcher_policy)
            .finish()
    }
}

impl std::fmt::Display for ScopedObservationAsyncOwnerPairBindFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ScopedObservationAsyncOwnerPairBindFailure {}

/// One structured future owner for the native callback/recovery backend and
/// the exact mutable source epoch. It does not spawn detached tasks: dropping
/// the pair still follows the native watcher and source-owner close contracts.
pub struct ScopedObservationAsyncOwnerPair {
    watcher: ScopedObservationNativeWatcher,
    source: ScopedObservationAsyncSourceOwner,
    watcher_policy: ScopedObservationNativeWatcherRecoveryPolicy,
}

impl std::fmt::Debug for ScopedObservationAsyncOwnerPair {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationAsyncOwnerPair")
            .field("watcher_phase", &self.watcher.coordinator.phase())
            .field("watcher_state", &self.watcher.state())
            .field("source_scope_epoch", &self.source.active.scope_epoch)
            .field("source_binding_count", &self.source.bindings.len())
            .field("watcher_policy", &self.watcher_policy)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedObservationAsyncOwnerFirstExit {
    Source,
    Watcher(ScopedObservationNativeWatcherRunExit),
    WatcherError(ScopedContinuityError),
}

/// Intentional continuity invalidation pauses only the source half. The
/// watcher backend, callback authority, recovery policy, and source handoff
/// remain owned together until replacement binds a fresh source owner.
pub struct ScopedObservationAsyncResyncHandoff {
    watcher: ScopedObservationNativeWatcher,
    source: ScopedObservationStoppedSourceOwner,
    watcher_policy: ScopedObservationNativeWatcherRecoveryPolicy,
}

impl ScopedObservationAsyncResyncHandoff {
    pub fn source(&self) -> &ScopedObservationStoppedSourceOwner {
        &self.source
    }

    pub fn into_parts(
        self,
    ) -> (
        ScopedObservationNativeWatcher,
        ScopedObservationStoppedSourceOwner,
        ScopedObservationNativeWatcherRecoveryPolicy,
    ) {
        (self.watcher, self.source, self.watcher_policy)
    }
}

impl std::fmt::Debug for ScopedObservationAsyncResyncHandoff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationAsyncResyncHandoff")
            .field("watcher_phase", &self.watcher.coordinator.phase())
            .field("watcher_state", &self.watcher.state())
            .field("source_exit", &self.source.exit())
            .field("source_binding_count", &self.source.binding_count())
            .field("watcher_policy", &self.watcher_policy)
            .finish_non_exhaustive()
    }
}

/// Terminal/cancelled paired-owner result. The native backend and watcher
/// registration have already been released; the stopped source state remains
/// available for bounded diagnostics, but cannot be resumed after failure.
pub struct ScopedObservationAsyncStoppedOwners {
    source: ScopedObservationStoppedSourceOwner,
    first_exit: ScopedObservationAsyncOwnerFirstExit,
    terminal_failure: Option<Arc<ScopedObserverFailure>>,
    supervision_error: Option<ScopedContinuityError>,
}

impl ScopedObservationAsyncStoppedOwners {
    pub fn source(&self) -> &ScopedObservationStoppedSourceOwner {
        &self.source
    }

    pub fn first_exit(&self) -> &ScopedObservationAsyncOwnerFirstExit {
        &self.first_exit
    }

    pub fn terminal_failure(&self) -> Option<Arc<ScopedObserverFailure>> {
        self.terminal_failure.as_ref().map(Arc::clone)
    }

    pub fn supervision_error(&self) -> Option<ScopedContinuityError> {
        self.supervision_error
    }

    pub fn into_source(self) -> ScopedObservationStoppedSourceOwner {
        self.source
    }
}

impl std::fmt::Debug for ScopedObservationAsyncStoppedOwners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationAsyncStoppedOwners")
            .field("source_exit", &self.source.exit())
            .field("source_binding_count", &self.source.binding_count())
            .field("first_exit", &self.first_exit)
            .field("terminal_failure", &self.terminal_failure)
            .field("supervision_error", &self.supervision_error)
            .finish_non_exhaustive()
    }
}

#[must_use = "a resync result retains watcher and source authority and must be rebound or deliberately dropped"]
pub enum ScopedObservationAsyncOwnerRunResult {
    Resync(ScopedObservationAsyncResyncHandoff),
    Stopped(ScopedObservationAsyncStoppedOwners),
}

impl std::fmt::Debug for ScopedObservationAsyncOwnerRunResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resync(handoff) => formatter.debug_tuple("Resync").field(handoff).finish(),
            Self::Stopped(stopped) => formatter.debug_tuple("Stopped").field(stopped).finish(),
        }
    }
}

enum ScopedObservationAsyncOwnerFirstCompletion {
    Source(Box<ScopedObservationStoppedSourceOwner>),
    Watcher(Result<ScopedObservationNativeWatcherRunExit, ScopedContinuityError>),
}

impl ScopedObservationAsyncOwnerPair {
    pub async fn run_until_stopped(self) -> ScopedObservationAsyncOwnerRunResult {
        self.run_until_stopped_inner(
            |callback| {
                let watcher = notify::recommended_watcher(callback).map_err(|_| ())?;
                Ok(Box::new(watcher))
            },
            scoped_observation_now_unix_ms,
            scoped_observation_now_unix_ms,
        )
        .await
    }

    async fn run_until_stopped_inner<F, W, S>(
        self,
        mut watcher_factory: F,
        mut watcher_observed_at: W,
        source_observed_at: S,
    ) -> ScopedObservationAsyncOwnerRunResult
    where
        F: FnMut(
                ScopedObservationNativeWatchCallback,
            ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>
            + Send,
        W: FnMut() -> i64 + Send,
        S: FnMut() -> i64 + Send,
    {
        let Self {
            mut watcher,
            source,
            watcher_policy,
        } = self;
        let mut watcher_future = Box::pin(watcher.run_with_recovery_loop(
            watcher_policy,
            &mut watcher_factory,
            &mut watcher_observed_at,
        ));
        let mut source_future = Box::pin(source.run_until_stopped_inner(source_observed_at));
        let first = tokio::select! {
            biased;
            exit = &mut watcher_future => {
                ScopedObservationAsyncOwnerFirstCompletion::Watcher(exit)
            }
            stopped = &mut source_future => {
                ScopedObservationAsyncOwnerFirstCompletion::Source(Box::new(stopped))
            }
        };

        match first {
            ScopedObservationAsyncOwnerFirstCompletion::Source(stopped) => {
                let stopped = *stopped;
                drop(source_future);
                drop(watcher_future);
                if matches!(
                    stopped.exit(),
                    ScopedObservationSourceOwnerRunExit::ContinuityInvalidated(_)
                ) {
                    return ScopedObservationAsyncOwnerRunResult::Resync(
                        ScopedObservationAsyncResyncHandoff {
                            watcher,
                            source: stopped,
                            watcher_policy,
                        },
                    );
                }

                let (terminal_failure, supervision_error) = if matches!(
                    stopped.exit(),
                    ScopedObservationSourceOwnerRunExit::Failed(_)
                ) {
                    match watcher.fail_observer(
                        ScopedObserverFailureReason::InternalControlFailure,
                        watcher_observed_at(),
                    ) {
                        Ok(failure) => (Some(failure), None),
                        Err(error) => (None, Some(error)),
                    }
                } else {
                    (None, None)
                };
                drop(watcher);
                ScopedObservationAsyncOwnerRunResult::Stopped(ScopedObservationAsyncStoppedOwners {
                    source: stopped,
                    first_exit: ScopedObservationAsyncOwnerFirstExit::Source,
                    terminal_failure,
                    supervision_error,
                })
            }
            ScopedObservationAsyncOwnerFirstCompletion::Watcher(exit) => {
                drop(watcher_future);
                let (first_exit, terminal_failure, supervision_error) = match exit {
                    Ok(exit @ ScopedObservationNativeWatcherRunExit::Cancelled) => (
                        ScopedObservationAsyncOwnerFirstExit::Watcher(exit),
                        None,
                        None,
                    ),
                    Ok(ScopedObservationNativeWatcherRunExit::Failed(failure)) => (
                        ScopedObservationAsyncOwnerFirstExit::Watcher(
                            ScopedObservationNativeWatcherRunExit::Failed(Arc::clone(&failure)),
                        ),
                        Some(failure),
                        None,
                    ),
                    Err(error) => {
                        let delivered = watcher.fail_observer(
                            ScopedObserverFailureReason::InternalControlFailure,
                            watcher_observed_at(),
                        );
                        let (terminal_failure, supervision_error) = match delivered {
                            Ok(failure) => (Some(failure), Some(error)),
                            Err(delivery_error) => (None, Some(delivery_error)),
                        };
                        (
                            ScopedObservationAsyncOwnerFirstExit::WatcherError(error),
                            terminal_failure,
                            supervision_error,
                        )
                    }
                };
                drop(watcher);
                let source = source_future.await;
                ScopedObservationAsyncOwnerRunResult::Stopped(ScopedObservationAsyncStoppedOwners {
                    source,
                    first_exit,
                    terminal_failure,
                    supervision_error,
                })
            }
        }
    }

    #[cfg(test)]
    pub async fn run_with_factory_and_clocks<F, W, S>(
        self,
        watcher_factory: F,
        watcher_observed_at: W,
        source_observed_at: S,
    ) -> ScopedObservationAsyncOwnerRunResult
    where
        F: FnMut(
                ScopedObservationNativeWatchCallback,
            ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>
            + Send,
        W: FnMut() -> i64 + Send,
        S: FnMut() -> i64 + Send,
    {
        self.run_until_stopped_inner(watcher_factory, watcher_observed_at, source_observed_at)
            .await
    }
}

impl ScopedObservationAsyncHandle {
    pub fn bind_live_owner_pair(
        &self,
        watcher: ScopedObservationNativeWatcher,
        source: ScopedObservationAsyncSourceOwner,
        watcher_policy: ScopedObservationNativeWatcherRecoveryPolicy,
    ) -> Result<ScopedObservationAsyncOwnerPair, ScopedObservationAsyncOwnerPairBindFailure> {
        let error = if !Arc::ptr_eq(&self.shared, &watcher.handle.shared)
            || !Arc::ptr_eq(&self.shared, &source.handle.shared)
        {
            Some(ScopedObservationAsyncOwnerPairBindingError::ForeignAttachment)
        } else if watcher.state().routing_failed || watcher.terminal_failure_delivered {
            Some(ScopedObservationAsyncOwnerPairBindingError::WatcherFailed)
        } else if !matches!(
            watcher.coordinator.phase(),
            ScopedObservationWatcherPhase::Live { .. }
        ) {
            Some(ScopedObservationAsyncOwnerPairBindingError::WatcherNotLive)
        } else if source.attachment_wait_context().is_err() {
            Some(ScopedObservationAsyncOwnerPairBindingError::SourceNotLive)
        } else {
            None
        };
        if let Some(error) = error {
            return Err(ScopedObservationAsyncOwnerPairBindFailure {
                error,
                watcher: Box::new(watcher),
                source: Box::new(source),
                watcher_policy,
            });
        }
        Ok(ScopedObservationAsyncOwnerPair {
            watcher,
            source,
            watcher_policy,
        })
    }

    pub fn install_native_watcher(
        &self,
        hint_capacity: usize,
    ) -> Result<ScopedObservationNativeWatcher, ScopedObservationNativeWatcherError> {
        self.install_native_watcher_with_factory_inner(hint_capacity, |callback| {
            let watcher = notify::recommended_watcher(callback).map_err(|_| ())?;
            Ok(Box::new(watcher))
        })
    }

    fn install_native_watcher_with_factory_inner<F>(
        &self,
        hint_capacity: usize,
        factory: F,
    ) -> Result<ScopedObservationNativeWatcher, ScopedObservationNativeWatcherError>
    where
        F: FnOnce(
            ScopedObservationNativeWatchCallback,
        ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>,
    {
        let coordinator = Arc::new(self.prepare_watcher_install(hint_capacity)?);
        let plan = Arc::new(ScopedObservationNativeWatchPlan::from_host(self.host())?);
        let completion = Arc::new(ScopedObservationNativeWatcherCompletion::default());
        let callback = scoped_native_watch_callback(
            self.clone(),
            Arc::clone(&coordinator),
            Arc::clone(&plan),
            Arc::clone(&completion),
        );
        let backend = install_scoped_native_watch_backend(&plan, callback, factory)?;
        let callback_state = completion.snapshot();
        if callback_state.backend_failed || callback_state.routing_failed {
            return Err(ScopedObservationNativeWatcherError::CallbackFailedDuringInstall);
        }
        coordinator.confirm_watcher_installed(self.host())?;
        completion.mark_initial_backend_installed();
        let watch_anchor_count = plan.anchors.len();
        Ok(ScopedObservationNativeWatcher {
            backend: Some(backend),
            handle: self.clone(),
            coordinator,
            plan,
            completion,
            watch_anchor_count,
            terminal_failure_delivered: false,
        })
    }

    #[cfg(test)]
    pub fn install_native_watcher_with_factory<F>(
        &self,
        hint_capacity: usize,
        factory: F,
    ) -> Result<ScopedObservationNativeWatcher, ScopedObservationNativeWatcherError>
    where
        F: FnOnce(
            ScopedObservationNativeWatchCallback,
        ) -> Result<Box<dyn ScopedObservationNativeWatchBackend>, ()>,
    {
        self.install_native_watcher_with_factory_inner(hint_capacity, factory)
    }
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
            ScopedProjectedObservation::SourceObjectError {
                observed_at, error, ..
            } => {
                if semantic_revision_ref.is_some()
                    || error.scope_epoch != scope_epoch
                    || error.source != delivered.source
                    || !error.validate()
                {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                ScopedMappedEnvelopeParts {
                    actor_run_key: self.root.root_actor_run_key,
                    actor_attribution: ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::SourceLifecycleControl,
                    },
                    source: scoped_control_envelope_source(
                        &delivered.source,
                        error.provenance.generation,
                    ),
                    native_time: None,
                    observed_at,
                    evidence: ScopedEnvelopeEvidence {
                        authority: ScopedEnvelopeEvidenceAuthority::EngineControl,
                        quality: QualifiedValueQuality::Derived,
                        effective_at: None,
                        completeness: if error.retry.is_terminal() {
                            ContractCompleteness::Unknown
                        } else {
                            ContractCompleteness::Partial
                        },
                    },
                    event: ScopedObservationEvent::SourceObjectError { error },
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
            ScopedProjectedObservation::ObserverFailed {
                observed_at,
                failure,
                ..
            } => {
                if semantic_revision_ref.is_some()
                    || failure.root != self.root
                    || failure.failed_scope_epoch != scope_epoch
                    || failure.control_sequence != observer_sequence
                    || failure.last_contiguous_sequence >= observer_sequence
                    || failure.phase != phase
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
                    event: ScopedObservationEvent::ObserverFailed { failure },
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopedObservationEventWakeState {
    pub offered_through_sequence: u64,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub struct ScopedObservationEventWaiter {
    completion: Arc<ScopedObservationEventCompletion>,
}

impl ScopedObservationEventWaiter {
    pub fn snapshot(&self) -> ScopedObservationEventWakeState {
        self.completion.snapshot()
    }

    /// Wait until an event with a strictly newer offered sequence exists or
    /// the owning drain closes. Passing a previously captured sequence avoids
    /// the check-then-sleep race that would otherwise lose a producer wakeup.
    pub fn wait_after(&self, offered_through_sequence: u64) -> ScopedObservationEventWakeState {
        self.completion.wait_after(offered_through_sequence)
    }

    /// Await a newer offered sequence or close without occupying a runtime
    /// worker. The notification retains its latest state, so an offer between
    /// future construction and first poll cannot be lost.
    pub async fn wait_after_async(
        &self,
        offered_through_sequence: u64,
    ) -> ScopedObservationEventWakeState {
        self.completion
            .wait_after_async(offered_through_sequence)
            .await
    }
}

#[derive(Debug)]
struct ScopedObservationEventCompletion {
    state: Mutex<ScopedObservationEventWakeState>,
    changed: Condvar,
    async_changed: tokio::sync::watch::Sender<ScopedObservationEventWakeState>,
}

impl Default for ScopedObservationEventCompletion {
    fn default() -> Self {
        let initial = ScopedObservationEventWakeState::default();
        let (async_changed, _) = tokio::sync::watch::channel(initial);
        Self {
            state: Mutex::new(initial),
            changed: Condvar::new(),
            async_changed,
        }
    }
}

impl ScopedObservationEventCompletion {
    fn snapshot(&self) -> ScopedObservationEventWakeState {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish(&self, offered_through_sequence: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        debug_assert!(offered_through_sequence >= state.offered_through_sequence);
        if offered_through_sequence > state.offered_through_sequence {
            state.offered_through_sequence = offered_through_sequence;
            self.async_changed.send_replace(*state);
            self.changed.notify_all();
        }
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.closed {
            state.closed = true;
            self.async_changed.send_replace(*state);
            self.changed.notify_all();
        }
    }

    fn wait_after(&self, offered_through_sequence: u64) -> ScopedObservationEventWakeState {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !state.closed && state.offered_through_sequence <= offered_through_sequence {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *state
    }

    async fn wait_after_async(
        &self,
        offered_through_sequence: u64,
    ) -> ScopedObservationEventWakeState {
        let mut changed = self.async_changed.subscribe();
        loop {
            let state = *changed.borrow_and_update();
            if state.closed || state.offered_through_sequence > offered_through_sequence {
                return state;
            }
            changed
                .changed()
                .await
                .expect("scoped event completion retains its async notification sender");
        }
    }
}

/// Retained producer-side notification for bounded delivery capacity. A
/// generation advances only when dequeue/invalidation releases or supersedes
/// queued work; ordinary offers do not advance it. Capturing the generation
/// before a producer attempt closes the dequeue-between-error-and-wait race.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopedObservationDeliveryCapacityState {
    pub generation: u128,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub struct ScopedObservationDeliveryCapacityWaiter {
    completion: Arc<ScopedObservationDeliveryCapacityCompletion>,
}

impl ScopedObservationDeliveryCapacityWaiter {
    pub fn snapshot(&self) -> ScopedObservationDeliveryCapacityState {
        self.completion.snapshot()
    }

    /// Await a dequeue/supersession after a generation captured before a
    /// producer attempt, or the owning drain's close. Retained watch state
    /// makes a release before first poll immediately visible.
    pub async fn wait_after_async(
        &self,
        generation: u128,
    ) -> ScopedObservationDeliveryCapacityState {
        self.completion.wait_after_async(generation).await
    }
}

#[derive(Debug)]
struct ScopedObservationDeliveryCapacityCompletion {
    state: Mutex<ScopedObservationDeliveryCapacityState>,
    async_changed: tokio::sync::watch::Sender<ScopedObservationDeliveryCapacityState>,
}

impl Default for ScopedObservationDeliveryCapacityCompletion {
    fn default() -> Self {
        let initial = ScopedObservationDeliveryCapacityState::default();
        let (async_changed, _) = tokio::sync::watch::channel(initial);
        Self {
            state: Mutex::new(initial),
            async_changed,
        }
    }
}

impl ScopedObservationDeliveryCapacityCompletion {
    fn snapshot(&self) -> ScopedObservationDeliveryCapacityState {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish_release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.closed {
            state.generation = state
                .generation
                .checked_add(1)
                .expect("scoped capacity generations cannot exhaust before observer sequences");
            self.async_changed.send_replace(*state);
        }
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.closed {
            state.closed = true;
            self.async_changed.send_replace(*state);
        }
    }

    async fn wait_after_async(&self, generation: u128) -> ScopedObservationDeliveryCapacityState {
        let mut changed = self.async_changed.subscribe();
        loop {
            let state = *changed.borrow_and_update();
            if state.closed || state.generation > generation {
                return state;
            }
            changed
                .changed()
                .await
                .expect("scoped capacity completion retains its async notification sender");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedObservationContinuity {
    Bootstrap,
    Valid,
    ResyncRequired,
    Resyncing,
    Failed,
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
    #[error("scoped coverage does not account for every authorized known-object relation")]
    DeclaredObjectCoverageMismatch,
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
    #[error("scoped observer is terminally failed")]
    ObserverFailed,
    #[error("scoped resync epoch accepts only correction-phase snapshot delivery")]
    InvalidResyncPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedBootstrapBarrierError {
    #[error("scoped bootstrap consumer drain belongs to another observer attachment")]
    ForeignDrain,
    #[error("scoped bootstrap consumer drain is closed")]
    Closed,
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
    #[error("scoped observation attachment is closed")]
    Closed,
    #[error("scoped observation operation accounting is exhausted")]
    OperationCapacityExhausted,
    #[error("scoped continuity cannot be invalidated before bootstrap completes")]
    BootstrapIncomplete,
    #[error("scoped observer is terminally failed")]
    ObserverFailed,
    #[error("scoped continuity control does not belong to the bound root")]
    RootMismatch,
    #[error("scoped continuity control identity is invalid")]
    InvalidControlIdentity,
    #[error("scoped resync has not been required")]
    ResyncNotRequired,
    #[error("scoped resync cannot start before resync-required is delivered")]
    ResyncRequiredNotDelivered,
    #[error("scoped replacement epoch cannot be invalidated before resync-start is delivered")]
    ResyncStartNotDelivered,
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
    observer_failure: Option<Arc<ScopedObserverFailure>>,
    event_completion: Arc<ScopedObservationEventCompletion>,
    capacity_completion: Arc<ScopedObservationDeliveryCapacityCompletion>,
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
            observer_failure: None,
            event_completion: Arc::new(ScopedObservationEventCompletion::default()),
            capacity_completion: Arc::new(ScopedObservationDeliveryCapacityCompletion::default()),
        })
    }

    fn discard_for_close(&mut self) {
        self.semantic.clear();
        self.source_controls.clear();
        self.queued_retained_native_bytes = 0;
        self.event_completion.close();
        self.capacity_completion.close();
    }

    fn event_waiter(&self) -> ScopedObservationEventWaiter {
        ScopedObservationEventWaiter {
            completion: Arc::clone(&self.event_completion),
        }
    }

    fn capacity_waiter(&self) -> ScopedObservationDeliveryCapacityWaiter {
        ScopedObservationDeliveryCapacityWaiter {
            completion: Arc::clone(&self.capacity_completion),
        }
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
        if self.observer_failure.is_some() {
            return Err(ScopedDeliveryOfferFailure {
                error: ScopedDeliveryError::ObserverFailed,
                projected,
            });
        }
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
        let offered_through_sequence = after_sequence
            .checked_sub(1)
            .expect("observer sequences begin at one");
        if first_offered_sequence.is_some() {
            self.event_completion.publish(offered_through_sequence);
        }

        Ok(ScopedObservationOfferReceipt {
            first_offered_sequence,
            offered_through_sequence,
            semantic_events: measurement.semantic_events as u64,
            retained_native_bytes: measurement.retained_native_bytes,
            source_control_items: measurement.source_control_items as u64,
        })
    }

    fn offer_bootstrap_barrier(
        &mut self,
        root: &ScopedObservationRootIdentity,
        watermark: ScopedObservationWatermarkCore,
        family_manifest: Vec<ScopedReplacementFamilyManifest>,
        root_present: bool,
        observed_at: i64,
    ) -> Result<Arc<ScopedBootstrapBarrier>, ScopedBootstrapBarrierError> {
        if self.resync_required.is_some() || self.observer_failure.is_some() {
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
        let replacement_snapshot_digest = replacement_snapshot_digest(
            root,
            root_present,
            &family_manifest,
            &watermark.source_coverage,
            &watermark.explicit_object_errors,
        )
        .map_err(|_| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        let barrier = Arc::new(ScopedBootstrapBarrier {
            barrier_contract_version: SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION,
            root: root.clone(),
            scope_epoch: before.scope_epoch,
            barrier_sequence,
            snapshot_digest,
            replacement_snapshot_digest,
            family_manifest,
            source_coverage: watermark.source_coverage,
            explicit_object_errors: watermark.explicit_object_errors,
            queue_state,
            root_present,
        });
        let source = observer_control_source(root)
            .map_err(|()| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        let event_id =
            bootstrap_complete_event_id(root, before.scope_epoch, replacement_snapshot_digest);
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
        if self.observer_failure.is_some() {
            return Err(ScopedContinuityError::ObserverFailed);
        }
        if let Some(started) = &self.resync_started {
            if started.root != *root {
                return Err(ScopedContinuityError::RootMismatch);
            }
            if self.delivered_through_sequence < started.control_sequence {
                return Err(ScopedContinuityError::ResyncStartNotDelivered);
            }
        } else if let Some(control) = &self.resync_required {
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
            .map_or(barrier.replacement_snapshot_digest, |value| {
                value.replacement_snapshot_digest
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
        self.resync_started = None;
        self.capacity_completion.publish_release();
        self.event_completion.publish(control_sequence);
        debug_assert_eq!(
            self.state().continuity,
            ScopedObservationContinuity::ResyncRequired
        );
        Ok(control)
    }

    fn fail_observer(
        &mut self,
        root: &ScopedObservationRootIdentity,
        reason: ScopedObserverFailureReason,
        observed_at: i64,
    ) -> Result<Arc<ScopedObserverFailure>, ScopedContinuityError> {
        if let Some(failure) = &self.observer_failure {
            return if failure.root == *root {
                Ok(Arc::clone(failure))
            } else {
                Err(ScopedContinuityError::RootMismatch)
            };
        }
        let control_sequence = self.next_observer_sequence;
        let after_sequence =
            control_sequence
                .checked_add(1)
                .ok_or(ScopedContinuityError::Delivery(
                    ScopedDeliveryError::ObserverSequenceExhausted,
                ))?;
        let discarded_semantic_events = u64::try_from(self.semantic.len())
            .map_err(|_| ScopedContinuityError::Delivery(ScopedDeliveryError::CapacityExhausted))?;
        let discarded_source_controls = u64::try_from(self.source_controls.len())
            .map_err(|_| ScopedContinuityError::Delivery(ScopedDeliveryError::CapacityExhausted))?;
        let phase = if self.resync_started.is_some() {
            ScopedAppendDeliveryPhase::Correction
        } else if self.bootstrap_barrier.is_some() {
            ScopedAppendDeliveryPhase::Live
        } else {
            ScopedAppendDeliveryPhase::Bootstrap
        };
        let source = observer_control_source(root)
            .map_err(|()| ScopedContinuityError::InvalidControlIdentity)?;
        let failure = Arc::new(ScopedObserverFailure {
            root: root.clone(),
            failed_scope_epoch: self.scope_epoch,
            control_sequence,
            last_contiguous_sequence: self.delivered_through_sequence,
            phase,
            reason,
            discarded_semantic_events,
            discarded_source_controls,
            discarded_retained_native_bytes: self.queued_retained_native_bytes,
        });
        let event_id = observer_failed_event_id(root, self.scope_epoch, reason);

        // A terminal failure is the final priority control for this
        // attachment. It explicitly supersedes every undelivered value and
        // incomplete resync control, then permanently rejects ordinary offers.
        self.semantic.clear();
        self.source_controls.clear();
        self.queued_retained_native_bytes = 0;
        self.resync_required = None;
        self.resync_started = None;
        self.source_controls.push_back(QueuedProjectedObservation {
            observer_sequence: control_sequence,
            retained_native_bytes: 0,
            value: ScopedProjectedObservation::ObserverFailed {
                source,
                observed_at,
                event_id,
                failure: Arc::clone(&failure),
            },
        });
        self.next_observer_sequence = after_sequence;
        self.observer_failure = Some(Arc::clone(&failure));
        self.capacity_completion.publish_release();
        self.event_completion.publish(control_sequence);
        debug_assert_eq!(self.state().continuity, ScopedObservationContinuity::Failed);
        Ok(failure)
    }

    fn begin_resync(
        &mut self,
        root: &ScopedObservationRootIdentity,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncStarted>, ScopedContinuityError> {
        if self.observer_failure.is_some() {
            return Err(ScopedContinuityError::ObserverFailed);
        }
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
        self.event_completion.publish(control_sequence);
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

    fn preview_next(&self) -> Option<ScopedDeliveredObservation> {
        let queued = self.next_queued()?;
        Some(ScopedDeliveredObservation {
            event_contract_version: SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION,
            observer_sequence: queued.observer_sequence,
            scope_epoch: self.scope_epoch,
            event_id: queued.value.event_id(),
            semantic_revision_ref: queued.value.semantic_revision_ref(),
            phase: queued.value.phase(),
            source: queued.value.source().clone(),
            event: queued.value.clone(),
        })
    }

    fn next_queued(&self) -> Option<&QueuedProjectedObservation> {
        match (self.source_controls.front(), self.semantic.front()) {
            (Some(control), Some(semantic)) => {
                Some(if control.observer_sequence < semantic.observer_sequence {
                    control
                } else {
                    semantic
                })
            }
            (Some(control), None) => Some(control),
            (None, Some(semantic)) => Some(semantic),
            (None, None) => None,
        }
    }

    fn dequeue_next(&mut self) -> Option<ScopedDeliveredObservation> {
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
        self.capacity_completion.publish_release();
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

    /// Test-only raw delivery seam. Production delivery must go through the
    /// claimed consumer drain so it cannot bypass the applied boundary.
    #[cfg(test)]
    pub fn pop_next(&mut self) -> Option<ScopedDeliveredObservation> {
        self.dequeue_next()
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
            continuity: if self.observer_failure.is_some() {
                ScopedObservationContinuity::Failed
            } else if self.resync_started.is_some() {
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

    pub fn observer_failure(&self) -> Option<Arc<ScopedObserverFailure>> {
        self.observer_failure.as_ref().map(Arc::clone)
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

impl Drop for ScopedObservationDeliveryLane {
    fn drop(&mut self) {
        self.event_completion.close();
        self.capacity_completion.close();
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
        | ScopedProjectedObservation::SourceObjectError { .. }
        | ScopedProjectedObservation::ObserverBootstrapComplete { .. }
        | ScopedProjectedObservation::ObserverResyncRequired { .. }
        | ScopedProjectedObservation::ObserverResyncStarted { .. }
        | ScopedProjectedObservation::ObserverResyncComplete { .. }
        | ScopedProjectedObservation::ObserverFailed { .. } => (false, 0),
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
    #[error("scoped replacement attachment is closed")]
    Closed,
    #[error("scoped replacement operation accounting is exhausted")]
    OperationCapacityExhausted,
    #[error("scoped replacement stage requires an active resync epoch")]
    NotResyncing,
    #[error("scoped replacement stage does not belong to the bound root")]
    RootMismatch,
    #[error("scoped replacement stage does not belong to the current epoch")]
    EpochMismatch,
    #[error("scoped replacement stage requires the current active reducer")]
    ActiveProjectionRequired,
    #[error("scoped replacement source state is missing or inconsistent")]
    InvalidSourceState,
    #[error("scoped replacement relation is not part of the bound source state")]
    UnknownSourceRelation,
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
        replacement_family_manifest(&prepared.usage_v2, source_coverage)
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

/// One complete active observer epoch at the store-free composition boundary.
/// Source objects, their offered coverage lane, and semantic reducers move as
/// one ownership unit during whole-scope replacement.
pub struct ScopedObservationEpochState {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    root: ScopedObservationRootIdentity,
    scope_epoch: u64,
    append_objects: BTreeMap<String, ScopedKnownAppendObject>,
    admission: ScopedObservationAdmissionLane,
    projection: ScopedObservationProjectionSink,
    object_errors: BTreeMap<String, ScopedSourceObjectErrorRuntime>,
}

#[derive(Debug, Clone)]
struct ScopedSourceObjectErrorRuntime {
    error: Arc<ScopedSourceObjectError>,
    observed_at: i64,
    control_offered: bool,
}

impl ScopedObservationEpochState {
    pub fn scope_epoch(&self) -> u64 {
        self.scope_epoch
    }

    pub fn append_object(&self, relation_id: &str) -> Option<&ScopedKnownAppendObject> {
        self.append_objects.get(relation_id)
    }

    pub fn admission_is_empty(&self) -> bool {
        self.admission.is_empty()
    }

    pub fn object_error(&self, relation_id: &str) -> Option<&ScopedSourceObjectError> {
        self.object_errors
            .get(relation_id)
            .map(|state| state.error.as_ref())
    }

    pub fn append_object_and_admission_mut(
        &mut self,
        relation_id: &str,
    ) -> Option<(
        &mut ScopedKnownAppendObject,
        &mut ScopedObservationAdmissionLane,
    )> {
        let object = self.append_objects.get_mut(relation_id)?;
        Some((object, &mut self.admission))
    }

    pub fn offer_next(
        &mut self,
        delivery: &mut ScopedObservationDeliveryLane,
    ) -> Result<Option<ScopedObservationOfferReceipt>, ScopedProjectionDeliveryError> {
        self.admission.offer_next(&mut self.projection, delivery)
    }
}

/// Whole-scope replacement owner. Replay mutates only these empty source,
/// coverage, and reducer components until one successful completion-control
/// offer transfers all three into `ScopedObservationEpochState`.
pub struct ScopedObservationScopeReplacementStage {
    semantic: ScopedObservationReplacementStage,
    append_objects: BTreeMap<String, ScopedKnownAppendObject>,
    admission: ScopedObservationAdmissionLane,
    activated: bool,
}

impl ScopedObservationScopeReplacementStage {
    pub fn scope_epoch(&self) -> u64 {
        self.semantic.scope_epoch
    }

    pub fn append_object_and_admission_mut(
        &mut self,
        relation_id: &str,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<
        (
            &mut ScopedKnownAppendObject,
            &mut ScopedObservationAdmissionLane,
        ),
        ScopedReplacementStageError,
    > {
        self.semantic.validate_delivery(delivery)?;
        if self.semantic.prepared.is_some() || self.activated {
            return Err(ScopedReplacementStageError::SnapshotAlreadyPrepared);
        }
        let object = self
            .append_objects
            .get_mut(relation_id)
            .ok_or(ScopedReplacementStageError::UnknownSourceRelation)?;
        Ok((object, &mut self.admission))
    }

    pub fn reduce_next(
        &mut self,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<bool, ScopedReplacementStageError> {
        self.semantic.validate_delivery(delivery)?;
        self.semantic.reduce_next(&mut self.admission)
    }

    pub fn prepare_snapshot(
        &mut self,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<&ScopedUsageV2ReplacementSnapshot, ScopedReplacementStageError> {
        self.semantic.prepare_snapshot(&self.admission, delivery)
    }

    pub fn offer_snapshot_next(
        &mut self,
        delivery: &mut ScopedObservationDeliveryLane,
    ) -> Result<Option<ScopedObservationOfferReceipt>, ScopedReplacementStageError> {
        self.semantic.offer_snapshot_next(delivery)
    }

    pub fn snapshot_fully_offered(&self) -> bool {
        self.semantic.snapshot_fully_offered()
    }

    pub fn append_object(&self, relation_id: &str) -> Option<&ScopedKnownAppendObject> {
        self.append_objects.get(relation_id)
    }

    pub fn is_activated(&self) -> bool {
        self.activated
    }

    pub fn admission_is_empty(&self) -> bool {
        self.admission.is_empty()
    }
}

fn replacement_family_manifest(
    usage_v2: &ScopedUsageV2ReplacementSnapshot,
    source_coverage: &[SourceCoverageSet],
) -> Result<Vec<ScopedReplacementFamilyManifest>, ScopedReplacementStageError> {
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
        return if usage_v2.entity_count == 0 {
            Ok(Vec::new())
        } else {
            Err(ScopedReplacementStageError::InvalidManifest)
        };
    };
    Ok(vec![ScopedReplacementFamilyManifest {
        fact_family: "runtime.usage-v2".to_string(),
        contract_version: usage_v2.fact_family_contract_version,
        replacement_representation:
            ScopedReplacementRepresentation::UsageLatestContributionPerResponse,
        completeness,
        entity_or_event_count: usage_v2.entity_count,
        semantic_digest: usage_v2.semantic_digest,
    }])
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

fn source_object_error_event_id(error: &ScopedSourceObjectError) -> ScopedObservationEventId {
    let mut hasher = source_control_event_hasher(&error.source, b"source.object-error");
    hasher.update(&error.error_contract_version.to_be_bytes());
    hash_event_component(&mut hasher, error.relation_id.as_bytes());
    hasher.update(&error.scope_epoch.to_be_bytes());
    hasher.update(&[error.failure_code.event_tag()]);
    hasher.update(&error.provenance.generation.to_be_bytes());
    match &error.provenance.last_successful_position {
        Some(position) => {
            hasher.update(&[1]);
            let encoded = serde_json::to_vec(position)
                .expect("validated common coverage positions always serialize");
            hash_event_component(&mut hasher, &encoded);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&[error.retry.event_tag()]);
    match error.retry {
        ScopedSourceObjectRetryState::RetryScheduled {
            failed_attempts,
            max_attempts,
            retry_after_ms,
        } => {
            hasher.update(&failed_attempts.to_be_bytes());
            hasher.update(&max_attempts.to_be_bytes());
            hasher.update(&retry_after_ms.to_be_bytes());
        }
        ScopedSourceObjectRetryState::RetryExhausted {
            failed_attempts,
            max_attempts,
        } => {
            hasher.update(&failed_attempts.to_be_bytes());
            hasher.update(&max_attempts.to_be_bytes());
        }
        ScopedSourceObjectRetryState::NotRetryable { failed_attempts } => {
            hasher.update(&failed_attempts.to_be_bytes());
        }
    }
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
    replacement_snapshot_digest: ScopedReplacementSnapshotDigest,
) -> ScopedObservationEventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"observer.bootstrap_complete");
    hash_event_component(&mut hasher, root.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, root.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, root.session_key.as_bytes());
    hasher.update(&scope_epoch.to_be_bytes());
    hash_event_component(&mut hasher, replacement_snapshot_digest.as_bytes());
    ScopedObservationEventId(*hasher.finalize().as_bytes())
}

fn resync_required_event_id(
    root: &ScopedObservationRootIdentity,
    scope_epoch: u64,
    reason: ScopedResyncReason,
    baseline_snapshot_digest: ScopedReplacementSnapshotDigest,
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

fn observer_failed_event_id(
    root: &ScopedObservationRootIdentity,
    scope_epoch: u64,
    reason: ScopedObserverFailureReason,
) -> ScopedObservationEventId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/observation-event-id\0");
    hasher.update(&SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION.to_be_bytes());
    hash_event_component(&mut hasher, b"observer.failed");
    hash_event_component(&mut hasher, root.adapter_id.as_str().as_bytes());
    hash_event_component(&mut hasher, root.source_instance_key.as_bytes());
    hash_event_component(&mut hasher, root.session_key.as_bytes());
    hasher.update(&scope_epoch.to_be_bytes());
    hasher.update(&[match reason {
        ScopedObserverFailureReason::NativeWatcherRecoveryExhausted => 1,
        ScopedObserverFailureReason::NativeWatcherRoutingFailed => 2,
        ScopedObserverFailureReason::InternalControlFailure => 3,
    }]);
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
    #[error(transparent)]
    ObservationContract(#[from] ObservationNegotiationError),
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
    #[error("scoped access pass sequence is exhausted")]
    AccessPassSequenceExhausted,
    #[error("scoped attachment operation accounting is exhausted")]
    OperationCapacityExhausted,
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
    #[error("scoped append object is not valid for this observer lifecycle")]
    InvalidObjectLifecycle,
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

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationStartupError {
    #[error("scoped observation host is closed")]
    Closed,
    #[error("scoped observation watcher orchestrator was already opened")]
    WatcherAlreadyInstalled,
    #[error("scoped observation watcher orchestrator belongs to another attachment")]
    ForeignHost,
    #[error("scoped observation startup pass belongs to another watcher orchestrator")]
    ForeignPass,
    #[error("scoped observation startup reconciliation already has an active pass")]
    ReconcilePassActive,
    #[error("scoped observation reconciliation batch limit must be greater than zero")]
    InvalidReconcileLimit,
    #[error("scoped observation bootstrap still has {pending_hints} watcher hint(s) to reconcile")]
    ReconcilePending { pending_hints: usize },
    #[error("cannot {operation} while scoped watcher startup phase is {phase:?}")]
    InvalidPhase {
        operation: &'static str,
        phase: ScopedObservationWatcherPhase,
    },
    #[error("scoped watcher startup ordering failed: {0}")]
    Ordering(#[source] SourceDriverError),
    #[error("scoped watcher startup access failed: {0}")]
    Access(#[from] ScopedObservationAccessError),
    #[error("scoped watcher startup coverage failed: {0}")]
    Poll(#[from] ScopedObservationPollError),
    #[error("scoped watcher bootstrap barrier failed: {0}")]
    Bootstrap(#[from] ScopedBootstrapBarrierError),
}

/// A request-local handle for one logical `poll()` call. Request generations
/// are flow-control coordinates only; they never enter event identity,
/// coverage, or a native source cursor.
#[derive(Debug, Clone)]
pub struct ScopedObservationPollTicket {
    runtime: Arc<ScopedObservationPollRuntime>,
    completion: Arc<ScopedObservationPollTicketCompletion>,
    request_generation: u64,
}

impl ScopedObservationPollTicket {
    pub fn request_generation(&self) -> u64 {
        self.request_generation
    }

    /// Blocking substrate for the future portable `poll()` future. A failed
    /// or dropped access pass leaves this pending; successful offered coverage
    /// or attachment close wakes every waiter exactly once.
    pub fn wait(&self) -> ScopedObservationPollResolution {
        self.completion.wait()
    }

    /// Await this request-local offered watermark without occupying a runtime
    /// worker. Completion and cancellation are retained across first poll.
    pub async fn wait_async(&self) -> ScopedObservationPollResolution {
        self.completion.wait_async().await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedObservationPollResolution {
    Pending,
    Ready(Arc<ScopedObservationWatermarkCore>),
    Failed(Arc<ScopedObserverFailure>),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedObservationReadyResolution {
    Pending,
    Ready(Arc<ScopedBootstrapBarrier>),
    Failed(Arc<ScopedObserverFailure>),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ScopedObservationReadyWaiter {
    completion: Arc<ScopedObservationReadyCompletion>,
}

impl ScopedObservationReadyWaiter {
    pub fn resolution(&self) -> ScopedObservationReadyResolution {
        self.completion.snapshot()
    }

    /// Blocking substrate for the future portable engine-level `ready()`
    /// future. Consumer-applied readiness remains a separate drain boundary.
    pub fn wait(&self) -> ScopedObservationReadyResolution {
        self.completion.wait()
    }

    /// Await the engine-offered bootstrap barrier without occupying a runtime
    /// worker. Consumer-applied readiness remains a separate drain boundary.
    pub async fn wait_async(&self) -> ScopedObservationReadyResolution {
        self.completion.wait_async().await
    }
}

#[derive(Debug)]
struct ScopedObservationReadyCompletion {
    resolution: Mutex<ScopedObservationReadyResolution>,
    changed: Condvar,
    async_changed: tokio::sync::watch::Sender<ScopedObservationReadyResolution>,
}

impl Default for ScopedObservationReadyCompletion {
    fn default() -> Self {
        let pending = ScopedObservationReadyResolution::Pending;
        let (async_changed, _) = tokio::sync::watch::channel(pending.clone());
        Self {
            resolution: Mutex::new(pending),
            changed: Condvar::new(),
            async_changed,
        }
    }
}

impl ScopedObservationReadyCompletion {
    fn snapshot(&self) -> ScopedObservationReadyResolution {
        self.resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn complete(&self, barrier: Arc<ScopedBootstrapBarrier>) {
        let mut resolution = self
            .resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*resolution {
            ScopedObservationReadyResolution::Pending => {
                *resolution = ScopedObservationReadyResolution::Ready(barrier);
                self.async_changed.send_replace(resolution.clone());
                self.changed.notify_all();
            }
            ScopedObservationReadyResolution::Ready(existing) => {
                debug_assert!(Arc::ptr_eq(existing, &barrier));
            }
            ScopedObservationReadyResolution::Failed(_)
            | ScopedObservationReadyResolution::Cancelled => {}
        }
    }

    fn cancel(&self) {
        let mut resolution = self
            .resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*resolution, ScopedObservationReadyResolution::Pending) {
            *resolution = ScopedObservationReadyResolution::Cancelled;
            self.async_changed.send_replace(resolution.clone());
            self.changed.notify_all();
        }
    }

    fn fail(&self, failure: Arc<ScopedObserverFailure>) {
        let mut resolution = self
            .resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*resolution, ScopedObservationReadyResolution::Pending) {
            *resolution = ScopedObservationReadyResolution::Failed(failure);
            self.async_changed.send_replace(resolution.clone());
            self.changed.notify_all();
        }
    }

    fn wait(&self) -> ScopedObservationReadyResolution {
        let mut resolution = self
            .resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while matches!(*resolution, ScopedObservationReadyResolution::Pending) {
            resolution = self
                .changed
                .wait(resolution)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        resolution.clone()
    }

    async fn wait_async(&self) -> ScopedObservationReadyResolution {
        let mut changed = self.async_changed.subscribe();
        loop {
            let resolution = changed.borrow_and_update().clone();
            if !matches!(resolution, ScopedObservationReadyResolution::Pending) {
                return resolution;
            }
            changed
                .changed()
                .await
                .expect("scoped ready completion retains its async notification sender");
        }
    }
}

#[derive(Debug)]
struct ScopedObservationPollTicketCompletion {
    resolution: Mutex<ScopedObservationPollResolution>,
    changed: Condvar,
    async_changed: tokio::sync::watch::Sender<ScopedObservationPollResolution>,
}

impl ScopedObservationPollTicketCompletion {
    fn snapshot(&self) -> ScopedObservationPollResolution {
        self.resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn resolve(&self, resolution: ScopedObservationPollResolution) {
        let mut current = self
            .resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*current, ScopedObservationPollResolution::Pending) {
            *current = resolution;
            self.async_changed.send_replace(current.clone());
            self.changed.notify_all();
        }
    }

    fn wait(&self) -> ScopedObservationPollResolution {
        let mut current = self
            .resolution
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while matches!(*current, ScopedObservationPollResolution::Pending) {
            current = self
                .changed
                .wait(current)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        current.clone()
    }

    async fn wait_async(&self) -> ScopedObservationPollResolution {
        let mut changed = self.async_changed.subscribe();
        loop {
            let resolution = changed.borrow_and_update().clone();
            if !matches!(resolution, ScopedObservationPollResolution::Pending) {
                return resolution;
            }
            changed
                .changed()
                .await
                .expect("scoped poll completion retains its async notification sender");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationPollState {
    pub requested_through_generation: u64,
    pub completed_through_generation: u64,
    pub in_flight_through_generation: Option<u64>,
    pub failed: bool,
    pub closed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationPollError {
    #[error("scoped observation host is closed")]
    Closed,
    #[error("scoped poll request or lease sequence is exhausted")]
    SequenceExhausted,
    #[error("scoped observer is terminally failed")]
    ObserverFailed,
    #[error("scoped poll ticket belongs to another observer attachment")]
    ForeignTicket,
    #[error("scoped poll lease belongs to another observer attachment")]
    ForeignLease,
    #[error("scoped consumer drain belongs to another observer attachment")]
    ForeignDrain,
    #[error(
        "scoped poll pass did not offer current coverage for every exact known-object relation"
    )]
    IncompleteScopePass,
    #[error("scoped poll lease no longer matches the active pass")]
    LeaseMismatch,
    #[error("scoped readiness completion does not match the attachment-owned delivery barrier")]
    ReadinessStateMismatch,
    #[error("scoped poll could not begin its bounded access pass: {0}")]
    Access(#[source] ScopedObservationAccessError),
    #[error("scoped poll could not publish an offered watermark: {0}")]
    Coverage(#[from] ScopedCoverageAssemblyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationSourceOwnerBindingError {
    #[error("scoped source-owner relation bindings are not the active exact relation set")]
    InvalidRelationSet,
    #[error("scoped source-owner identity input is invalid")]
    InvalidIdentityInput,
    #[error("scoped source-owner access identity differs from the bootstrap binding")]
    AccessIdentityMismatch,
    #[error("scoped source-owner access bounds are invalid")]
    InvalidBounds,
    #[error("scoped source owner requires the attachment's current valid epoch")]
    InvalidEpochState,
    #[error("scoped source-owner retry policy is incompatible with retained scheduled work")]
    RetryPolicyMismatch,
    #[error("scoped source owner could not recover the attachment's authorized scope program")]
    AuthorizedProgramUnavailable,
    #[error("scoped source owner cannot start after attachment close")]
    Closed,
    #[error("scoped source-owner operation accounting is exhausted")]
    OperationCapacityExhausted,
}

/// Failed ownership transfer retains both the epoch and redacted owned
/// bindings so callers can correct a configuration error without rebuilding
/// or silently losing live source state.
pub struct ScopedObservationSourceOwnerBindFailure {
    error: ScopedObservationSourceOwnerBindingError,
    active: Box<ScopedObservationEpochState>,
    bindings: Vec<ScopedObservationAppendPassBinding>,
}

impl ScopedObservationSourceOwnerBindFailure {
    pub fn error(&self) -> ScopedObservationSourceOwnerBindingError {
        self.error
    }

    pub fn into_parts(
        self,
    ) -> (
        ScopedObservationSourceOwnerBindingError,
        ScopedObservationEpochState,
        Vec<ScopedObservationAppendPassBinding>,
    ) {
        (self.error, *self.active, self.bindings)
    }
}

impl std::fmt::Debug for ScopedObservationSourceOwnerBindFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationSourceOwnerBindFailure")
            .field("error", &self.error)
            .field("scope_epoch", &self.active.scope_epoch)
            .field("bindings", &self.bindings)
            .finish()
    }
}

impl std::fmt::Display for ScopedObservationSourceOwnerBindFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ScopedObservationSourceOwnerBindFailure {}

/// Owned native identity input retained only by the trusted source owner.
/// Debug output deliberately omits the value; borrowed pass requests expose
/// only the declaration's input names as well.
#[derive(Clone, PartialEq, Eq)]
pub struct ScopedObservationOwnedIdentityInput {
    name: String,
    value: Vec<u8>,
}

impl ScopedObservationOwnedIdentityInput {
    pub fn new(
        name: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Result<Self, ScopedObservationSourceOwnerBindingError> {
        let name = name.into();
        let value = value.into();
        if name.trim().is_empty() || value.is_empty() || value.len() > MAX_IDENTITY_VALUE_BYTES {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidIdentityInput);
        }
        Ok(Self { name, value })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for ScopedObservationOwnedIdentityInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationOwnedIdentityInput")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Long-lived owned binding for one exact append relation. The binding fixes
/// native identity, hierarchy, bounded access, record-origin lineage, and
/// replay policy for the lifetime of one active epoch owner.
#[derive(Clone)]
pub struct ScopedObservationAppendPassBinding {
    relation_id: String,
    identity_inputs: Vec<ScopedObservationOwnedIdentityInput>,
    parent_token: Option<AccessObjectToken>,
    depth: u32,
    max_bytes: u64,
    origin: RecordOrigin,
    force_contract_replay: bool,
}

impl ScopedObservationAppendPassBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        relation_id: impl Into<String>,
        identity_inputs: Vec<ScopedObservationOwnedIdentityInput>,
        parent_token: Option<AccessObjectToken>,
        depth: u32,
        max_bytes: u64,
        origin: RecordOrigin,
        force_contract_replay: bool,
    ) -> Result<Self, ScopedObservationSourceOwnerBindingError> {
        let relation_id = relation_id.into();
        if relation_id.trim().is_empty() || identity_inputs.is_empty() {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidRelationSet);
        }
        if identity_inputs.iter().enumerate().any(|(index, input)| {
            identity_inputs[..index]
                .iter()
                .any(|prior| prior.name == input.name)
        }) {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidIdentityInput);
        }
        if depth == 0 || max_bytes == 0 {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidBounds);
        }
        Ok(Self {
            relation_id,
            identity_inputs,
            parent_token,
            depth,
            max_bytes,
            origin,
            force_contract_replay,
        })
    }

    pub fn relation_id(&self) -> &str {
        &self.relation_id
    }
}

impl std::fmt::Debug for ScopedObservationAppendPassBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationAppendPassBinding")
            .field("relation_id", &self.relation_id)
            .field(
                "identity_input_names",
                &self
                    .identity_inputs
                    .iter()
                    .map(ScopedObservationOwnedIdentityInput::name)
                    .collect::<Vec<_>>(),
            )
            .field("has_parent_token", &self.parent_token.is_some())
            .field("depth", &self.depth)
            .field("max_bytes", &self.max_bytes)
            .field(
                "has_source_timestamp_hint",
                &self.origin.source_timestamp_hint.is_some(),
            )
            .field("force_contract_replay", &self.force_contract_replay)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopedObservationSourceOwnerRetryPolicyError {
    #[error("scoped source-owner retry interval is outside the supported bound")]
    RetryInterval,
    #[error("scoped source-owner retry attempt limit is outside the supported bound")]
    AttemptLimit,
}

/// Bounded retry policy for genuinely transient source/decode failures. Queue
/// pressure is not timer-retried: it waits for the exact delivery owner to
/// release capacity. Values remain internal policy, not an RFC performance
/// gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedObservationSourceOwnerRetryPolicy {
    initial_retry_delay: Duration,
    max_retry_delay: Duration,
    max_transient_attempts: u32,
}

impl ScopedObservationSourceOwnerRetryPolicy {
    const MAX_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);
    const MAX_TRANSIENT_ATTEMPTS: u32 = 32;

    pub fn new(
        initial_retry_delay: Duration,
        max_retry_delay: Duration,
        max_transient_attempts: u32,
    ) -> Result<Self, ScopedObservationSourceOwnerRetryPolicyError> {
        if initial_retry_delay.is_zero()
            || initial_retry_delay > max_retry_delay
            || max_retry_delay > Self::MAX_RETRY_DELAY
        {
            return Err(ScopedObservationSourceOwnerRetryPolicyError::RetryInterval);
        }
        if max_transient_attempts == 0 || max_transient_attempts > Self::MAX_TRANSIENT_ATTEMPTS {
            return Err(ScopedObservationSourceOwnerRetryPolicyError::AttemptLimit);
        }
        Ok(Self {
            initial_retry_delay,
            max_retry_delay,
            max_transient_attempts,
        })
    }

    fn retry_delay(self, completed_attempts: u32) -> Duration {
        let exponent = completed_attempts.min(31);
        self.initial_retry_delay
            .saturating_mul(1_u32 << exponent)
            .min(self.max_retry_delay)
    }

    fn accepts_retained_retry(self, retry: ScopedSourceObjectRetryState) -> bool {
        match retry {
            ScopedSourceObjectRetryState::RetryScheduled {
                max_attempts,
                retry_after_ms,
                ..
            } => {
                max_attempts == self.max_transient_attempts
                    && retry_after_ms
                        <= u64::try_from(self.max_retry_delay.as_millis())
                            .expect("the bounded retry duration fits u64 milliseconds")
            }
            ScopedSourceObjectRetryState::RetryExhausted { .. }
            | ScopedSourceObjectRetryState::NotRetryable { .. } => true,
        }
    }
}

impl Default for ScopedObservationSourceOwnerRetryPolicy {
    fn default() -> Self {
        Self {
            initial_retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
            max_transient_attempts: 5,
        }
    }
}

/// One trusted source-owner binding for an exact append relation in a live
/// poll pass. Native identity values remain borrowed and never enter the
/// access report or observer envelope.
#[derive(Clone, Copy)]
pub struct ScopedObservationAppendPassRequest<'a> {
    pub relation_id: &'a str,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub parent_token: Option<AccessObjectToken>,
    pub depth: u32,
    pub max_bytes: u64,
    pub origin: &'a RecordOrigin,
    pub force_contract_replay: bool,
}

impl std::fmt::Debug for ScopedObservationAppendPassRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationAppendPassRequest")
            .field("relation_id", &self.relation_id)
            .field(
                "identity_input_names",
                &self
                    .identity_inputs
                    .iter()
                    .map(|input| input.name)
                    .collect::<Vec<_>>(),
            )
            .field("has_parent_token", &self.parent_token.is_some())
            .field("depth", &self.depth)
            .field("max_bytes", &self.max_bytes)
            .field("force_contract_replay", &self.force_contract_replay)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationPassExecutionError {
    #[error("scoped poll executor requires exactly one request for every active relation")]
    InvalidRelationSet,
    #[error("scoped poll executor requires the attachment's current valid epoch")]
    InvalidEpochState,
    #[error("scoped append decode requested a retry without advancing source state")]
    DecodeRetryTransient,
    #[error("scoped poll source access failed: {0}")]
    Access(#[from] ScopedObservationAccessError),
    #[error("scoped poll admission failed: {0}")]
    Admission(ScopedAdmissionError),
    #[error("scoped poll offered-boundary delivery failed: {0}")]
    Offer(#[from] ScopedObservationConsumerOfferError),
    #[error("scoped poll completion failed: {0}")]
    Poll(#[from] ScopedObservationPollError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedObservationRelationPollOutcome {
    Ready,
    RetryTransient,
}

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationSourceOwnerRunError {
    #[error("scoped source-owner pass failed: {0}")]
    Pass(#[source] ScopedObservationPassExecutionError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedObservationSourceOwnerContinuityInvalidation {
    owned_scope_epoch: u64,
    observed_scope_epoch: u64,
    observed_continuity: ScopedObservationContinuity,
    control: Option<Arc<ScopedResyncRequired>>,
}

impl ScopedObservationSourceOwnerContinuityInvalidation {
    pub fn owned_scope_epoch(&self) -> u64 {
        self.owned_scope_epoch
    }

    pub fn observed_scope_epoch(&self) -> u64 {
        self.observed_scope_epoch
    }

    pub fn observed_continuity(&self) -> ScopedObservationContinuity {
        self.observed_continuity
    }

    /// The exact invalidation control is retained when the owner observes its
    /// own epoch being invalidated. A late owner that is not scheduled until a
    /// later replacement completes still exits through the epoch mismatch,
    /// but the delivery lane may already have retired that transient control.
    pub fn control(&self) -> Option<Arc<ScopedResyncRequired>> {
        self.control.as_ref().map(Arc::clone)
    }
}

#[derive(Debug)]
pub enum ScopedObservationSourceOwnerRunExit {
    Cancelled,
    ContinuityInvalidated(ScopedObservationSourceOwnerContinuityInvalidation),
    Failed(ScopedObservationSourceOwnerRunError),
}

struct ScopedObservationSourceOwnerWaitContext {
    event_waiter: ScopedObservationEventWaiter,
    offered_through_sequence: u64,
}

/// Result of a stopped source owner. Its attachment operation has already
/// been released, so close can complete even while the caller retains this
/// value for epoch recovery or diagnostics.
pub struct ScopedObservationStoppedSourceOwner {
    active: ScopedObservationEpochState,
    bindings: Vec<ScopedObservationAppendPassBinding>,
    policy: ScopedObservationSourceOwnerRetryPolicy,
    exit: ScopedObservationSourceOwnerRunExit,
}

impl ScopedObservationStoppedSourceOwner {
    pub fn exit(&self) -> &ScopedObservationSourceOwnerRunExit {
        &self.exit
    }

    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }

    pub fn retry_policy(&self) -> ScopedObservationSourceOwnerRetryPolicy {
        self.policy
    }

    pub fn into_parts(
        self,
    ) -> (
        ScopedObservationEpochState,
        ScopedObservationSourceOwnerRunExit,
    ) {
        (self.active, self.exit)
    }

    /// Recover the inseparable epoch, redacted owned relation bindings, and
    /// retry policy needed to bind a fresh source owner after replacement.
    pub fn into_rebind_parts(
        self,
    ) -> (
        ScopedObservationEpochState,
        Vec<ScopedObservationAppendPassBinding>,
        ScopedObservationSourceOwnerRetryPolicy,
        ScopedObservationSourceOwnerRunExit,
    ) {
        (self.active, self.bindings, self.policy, self.exit)
    }
}

fn scoped_source_owner_error_is_delivery_backpressure(
    error: &ScopedObservationPassExecutionError,
) -> bool {
    matches!(
        error,
        ScopedObservationPassExecutionError::Offer(ScopedObservationConsumerOfferError::Offer(
            ScopedProjectionDeliveryError::Delivery(
                ScopedDeliveryError::SemanticQueueFull
                    | ScopedDeliveryError::RetainedNativeQueueFull
                    | ScopedDeliveryError::SourceControlQueueFull
            )
        ))
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedObjectFailureClassification {
    Retryable(ScopedSourceObjectFailureCode),
    Terminal(ScopedSourceObjectFailureCode),
}

fn scoped_object_failure_classification(
    error: &ScopedObservationPassExecutionError,
) -> Option<ScopedObjectFailureClassification> {
    use ScopedObjectFailureClassification::{Retryable, Terminal};
    use ScopedSourceObjectFailureCode::{
        DecodeRecordPermanent, DecodeRetryTransient, DecodeStreamFatal, SourceDatabase,
        SourceInvalidConfiguration, SourceInvalidCursor, SourceIo, SourceLimitExceeded,
        SourcePathEscape, SourceUnstable,
    };

    match error {
        ScopedObservationPassExecutionError::DecodeRetryTransient
        | ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Decode(
            ScopedDecodeFailureClass::Transient,
        )) => Some(Retryable(DecodeRetryTransient)),
        ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Source(
            source_failure,
        )) => Some(match source_failure {
            ScopedSourceFailureClass::Unstable => Retryable(SourceUnstable),
            ScopedSourceFailureClass::Database => Retryable(SourceDatabase),
            ScopedSourceFailureClass::Io => Retryable(SourceIo),
            ScopedSourceFailureClass::InvalidConfiguration => Terminal(SourceInvalidConfiguration),
            ScopedSourceFailureClass::InvalidCursor => Terminal(SourceInvalidCursor),
            ScopedSourceFailureClass::PathEscape => Terminal(SourcePathEscape),
            ScopedSourceFailureClass::LimitExceeded => Terminal(SourceLimitExceeded),
        }),
        ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Decode(
            ScopedDecodeFailureClass::RecordPermanent,
        )) => Some(Terminal(DecodeRecordPermanent)),
        ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Decode(
            ScopedDecodeFailureClass::StreamFatal,
        )) => Some(Terminal(DecodeStreamFatal)),
        ScopedObservationPassExecutionError::InvalidRelationSet
        | ScopedObservationPassExecutionError::InvalidEpochState
        | ScopedObservationPassExecutionError::Access(_)
        | ScopedObservationPassExecutionError::Admission(_)
        | ScopedObservationPassExecutionError::Offer(_)
        | ScopedObservationPassExecutionError::Poll(_) => None,
    }
}

fn scoped_source_owner_error_is_cancelled(error: &ScopedObservationPassExecutionError) -> bool {
    matches!(
        error,
        ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Closed)
            | ScopedObservationPassExecutionError::Offer(
                ScopedObservationConsumerOfferError::Closed
            )
            | ScopedObservationPassExecutionError::Poll(ScopedObservationPollError::Closed)
    )
}

#[derive(Debug, Clone, Copy)]
struct ScopedObservationActivePoll {
    lease_id: u64,
    target_generation: u64,
}

#[derive(Debug, Default)]
struct ScopedObservationPollRuntimeState {
    requested_through_generation: u64,
    completed_through_generation: u64,
    next_lease_id: u64,
    active: Option<ScopedObservationActivePoll>,
    pending_completions: BTreeMap<u64, Weak<ScopedObservationPollTicketCompletion>>,
    failure: Option<Arc<ScopedObserverFailure>>,
    closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScopedObservationPollDriverResolution {
    Pending,
    WorkAvailable,
    Failed,
    Closed,
}

#[derive(Debug)]
struct ScopedObservationPollRuntime {
    state: Mutex<ScopedObservationPollRuntimeState>,
    driver_changed: tokio::sync::watch::Sender<ScopedObservationPollDriverResolution>,
}

impl Default for ScopedObservationPollRuntime {
    fn default() -> Self {
        let (driver_changed, _) =
            tokio::sync::watch::channel(ScopedObservationPollDriverResolution::Pending);
        Self {
            state: Mutex::new(ScopedObservationPollRuntimeState::default()),
            driver_changed,
        }
    }
}

impl ScopedObservationPollRuntime {
    fn lock_state(&self) -> MutexGuard<'_, ScopedObservationPollRuntimeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn driver_resolution(
        state: &ScopedObservationPollRuntimeState,
    ) -> ScopedObservationPollDriverResolution {
        if state.closed {
            ScopedObservationPollDriverResolution::Closed
        } else if state.failure.is_some() {
            ScopedObservationPollDriverResolution::Failed
        } else if state.active.is_none()
            && state.requested_through_generation > state.completed_through_generation
        {
            ScopedObservationPollDriverResolution::WorkAvailable
        } else {
            ScopedObservationPollDriverResolution::Pending
        }
    }

    fn publish_driver_resolution(&self, state: &ScopedObservationPollRuntimeState) {
        self.driver_changed
            .send_replace(Self::driver_resolution(state));
    }

    fn driver_waiter(&self) -> ScopedObservationPollDriverWaiter {
        ScopedObservationPollDriverWaiter {
            changed: self.driver_changed.subscribe(),
        }
    }

    fn request(
        self: &Arc<Self>,
    ) -> Result<ScopedObservationPollTicket, ScopedObservationPollError> {
        let mut state = self.lock_state();
        if state.closed {
            return Err(ScopedObservationPollError::Closed);
        }
        if state.failure.is_some() {
            return Err(ScopedObservationPollError::ObserverFailed);
        }
        state.requested_through_generation = state
            .requested_through_generation
            .checked_add(1)
            .ok_or(ScopedObservationPollError::SequenceExhausted)?;
        let request_generation = state.requested_through_generation;
        let pending = ScopedObservationPollResolution::Pending;
        let (async_changed, _) = tokio::sync::watch::channel(pending.clone());
        let completion = Arc::new(ScopedObservationPollTicketCompletion {
            resolution: Mutex::new(pending),
            changed: Condvar::new(),
            async_changed,
        });
        state
            .pending_completions
            .retain(|_, completion| completion.strong_count() > 0);
        state
            .pending_completions
            .insert(request_generation, Arc::downgrade(&completion));
        self.publish_driver_resolution(&state);
        Ok(ScopedObservationPollTicket {
            runtime: Arc::clone(self),
            completion,
            request_generation,
        })
    }

    /// Reserve all work requested before this instant for one bounded pass.
    /// A request arriving after reservation advances the requested generation
    /// and therefore remains pending for a follow-up pass.
    fn reserve(
        self: &Arc<Self>,
    ) -> Result<Option<ScopedObservationActivePoll>, ScopedObservationPollError> {
        let mut state = self.lock_state();
        if state.closed {
            return Err(ScopedObservationPollError::Closed);
        }
        if state.failure.is_some() {
            return Err(ScopedObservationPollError::ObserverFailed);
        }
        if state.active.is_some()
            || state.requested_through_generation == state.completed_through_generation
        {
            return Ok(None);
        }
        state.next_lease_id = state
            .next_lease_id
            .checked_add(1)
            .ok_or(ScopedObservationPollError::SequenceExhausted)?;
        let active = ScopedObservationActivePoll {
            lease_id: state.next_lease_id,
            target_generation: state.requested_through_generation,
        };
        state.active = Some(active);
        self.publish_driver_resolution(&state);
        Ok(Some(active))
    }

    fn abandon(&self, lease_id: u64, target_generation: u64) {
        let mut state = self.lock_state();
        if state.active.is_some_and(|active| {
            active.lease_id == lease_id && active.target_generation == target_generation
        }) {
            state.active = None;
            self.publish_driver_resolution(&state);
        }
    }

    fn complete(
        &self,
        lease_id: u64,
        target_generation: u64,
        watermark: Arc<ScopedObservationWatermarkCore>,
    ) -> Result<Arc<ScopedObservationWatermarkCore>, ScopedObservationPollError> {
        let mut state = self.lock_state();
        if state.closed {
            return Err(ScopedObservationPollError::Closed);
        }
        if state.failure.is_some() {
            return Err(ScopedObservationPollError::ObserverFailed);
        }
        let Some(active) = state.active else {
            return Err(ScopedObservationPollError::LeaseMismatch);
        };
        if active.lease_id != lease_id || active.target_generation != target_generation {
            return Err(ScopedObservationPollError::LeaseMismatch);
        }
        let completed = state
            .pending_completions
            .range(..=target_generation)
            .filter_map(|(generation, completion)| {
                completion
                    .upgrade()
                    .map(|completion| (*generation, completion))
            })
            .collect::<Vec<_>>();
        for (generation, completion) in completed {
            completion.resolve(ScopedObservationPollResolution::Ready(Arc::clone(
                &watermark,
            )));
            state.pending_completions.remove(&generation);
        }
        state
            .pending_completions
            .retain(|generation, _| *generation > target_generation);
        state.completed_through_generation = target_generation;
        state.active = None;
        self.publish_driver_resolution(&state);
        Ok(watermark)
    }

    fn resolution(
        self: &Arc<Self>,
        ticket: &ScopedObservationPollTicket,
    ) -> Result<ScopedObservationPollResolution, ScopedObservationPollError> {
        if !Arc::ptr_eq(&ticket.runtime, self) {
            return Err(ScopedObservationPollError::ForeignTicket);
        }
        Ok(ticket.completion.snapshot())
    }

    fn snapshot(&self) -> ScopedObservationPollState {
        let state = self.lock_state();
        ScopedObservationPollState {
            requested_through_generation: state.requested_through_generation,
            completed_through_generation: state.completed_through_generation,
            in_flight_through_generation: state.active.map(|active| active.target_generation),
            failed: state.failure.is_some(),
            closed: state.closed,
        }
    }

    fn close(&self) {
        let mut state = self.lock_state();
        state.closed = true;
        for completion in state.pending_completions.values().filter_map(Weak::upgrade) {
            completion.resolve(ScopedObservationPollResolution::Cancelled);
        }
        state.pending_completions.clear();
        self.publish_driver_resolution(&state);
    }

    fn fail(&self, failure: Arc<ScopedObserverFailure>) {
        let mut state = self.lock_state();
        if state.closed || state.failure.is_some() {
            return;
        }
        state.failure = Some(Arc::clone(&failure));
        state.active = None;
        for completion in state.pending_completions.values().filter_map(Weak::upgrade) {
            completion.resolve(ScopedObservationPollResolution::Failed(Arc::clone(
                &failure,
            )));
        }
        state.pending_completions.clear();
        self.publish_driver_resolution(&state);
    }
}

#[derive(Debug)]
struct ScopedObservationPollDriverWaiter {
    changed: tokio::sync::watch::Receiver<ScopedObservationPollDriverResolution>,
}

impl ScopedObservationPollDriverWaiter {
    async fn wait(&mut self) -> ScopedObservationPollDriverResolution {
        loop {
            let resolution = self.changed.borrow_and_update().clone();
            if !matches!(resolution, ScopedObservationPollDriverResolution::Pending) {
                return resolution;
            }
            self.changed
                .changed()
                .await
                .expect("scoped poll runtime retains its driver notification sender");
        }
    }
}

struct ScopedObservationAccessState {
    closed: AtomicBool,
    pass_active: AtomicBool,
    next_pass_id: AtomicU64,
    consumer_drain_opened: AtomicBool,
    watcher_orchestrator_opened: AtomicBool,
    poll: Arc<ScopedObservationPollRuntime>,
    ready: Arc<ScopedObservationReadyCompletion>,
}

/// One serialized bounded access pass satisfying every poll request through
/// `target_generation`. Dropping it before successful watermark publication
/// requeues that target; no ticket is acknowledged by merely starting or
/// reading a pass.
pub struct ScopedObservationPollLease {
    runtime: Arc<ScopedObservationPollRuntime>,
    lease_id: u64,
    target_generation: u64,
    access_pass: Option<ScopedObservationAccessPass>,
    completed: bool,
}

impl ScopedObservationPollLease {
    pub fn target_generation(&self) -> u64 {
        self.target_generation
    }

    pub fn access_pass(&self) -> &ScopedObservationAccessPass {
        self.access_pass
            .as_ref()
            .expect("an unfinished scoped poll lease retains its access pass")
    }

    fn complete(
        mut self,
        watermark: ScopedObservationWatermarkCore,
    ) -> Result<Arc<ScopedObservationWatermarkCore>, ScopedObservationPollError> {
        // Release the serialized native-access slot before advertising that a
        // follow-up poll is runnable. An async driver may react to the runtime
        // notification immediately on another task.
        drop(self.access_pass.take());
        let watermark =
            self.runtime
                .complete(self.lease_id, self.target_generation, Arc::new(watermark))?;
        self.completed = true;
        Ok(watermark)
    }
}

impl Drop for ScopedObservationPollLease {
    fn drop(&mut self) {
        if !self.completed {
            // Release native-access serialization before making the abandoned
            // target runnable for another driver.
            drop(self.access_pass.take());
            self.runtime.abandon(self.lease_id, self.target_generation);
        }
    }
}

/// The database-free scoped composition root owns the unforgeable typed
/// authorization and exact grants. It creates a fresh access ledger only at a
/// common-runtime pass boundary and never exposes the authorization itself.
pub struct ScopedObservationAccessHost {
    adapter: Arc<dyn AgentAdapter>,
    compatibility: CompatibilityDecision,
    observation_contract: ObservationContractSelection,
    authorization: TypedAccessAuthorization,
    root_identity: ScopedObservationRootIdentity,
    program_id: String,
    known_objects: Arc<BTreeMap<String, ScopedKnownObjectGrant>>,
    root_relation_id: Arc<str>,
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    lifecycle: Arc<ScopedObservationAttachmentLifecycle>,
    state: Arc<ScopedObservationAccessState>,
}

impl ScopedObservationAccessHost {
    pub fn authorize(
        registry: &AdapterRegistry,
        request: ScopedObservationAccessRequest,
    ) -> Result<Self, ScopedObservationAccessError> {
        // Contract selection is the first operation at this composition
        // boundary. Incompatible semantics therefore fail before support
        // classification, grant validation, or construction of any source-
        // access authority.
        let observation_contract = negotiate_observation_contract(
            &request.observation_contract_request,
            &request.observation_contract_offer,
        )?;
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
                &request.observation_contract_request.contract_versions,
                &request.observation_contract_offer.contract_versions,
            )
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        if authorization.contracts() != &observation_contract.contract_versions {
            return Err(ScopedObservationAccessError::Authorization(
                "typed access authorization does not match the negotiated observation contract"
                    .to_string(),
            ));
        }
        let root_identity = request.root_identity.resolve(
            &adapter_id,
            observation_contract
                .contract_versions
                .external_entity_reference_version,
        )?;
        let program = authorization
            .select_scope_program(&request.program_id)
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program)?;
        let known_objects = validate_known_object_grants(&plan, request.known_objects)?;
        let root_relation_id: Arc<str> = Arc::from(
            known_objects
                .values()
                .find(|grant| grant.scope_root)
                .expect("validated known-object grants contain exactly one scope root")
                .relation_id
                .as_str(),
        );
        Ok(Self {
            adapter,
            compatibility,
            observation_contract,
            authorization,
            root_identity,
            program_id: request.program_id,
            known_objects: Arc::new(known_objects),
            root_relation_id,
            attachment_authority: Arc::new(ScopedObservationAttachmentAuthority),
            lifecycle: Arc::new(ScopedObservationAttachmentLifecycle::default()),
            state: Arc::new(ScopedObservationAccessState {
                closed: AtomicBool::new(false),
                pass_active: AtomicBool::new(false),
                next_pass_id: AtomicU64::new(1),
                consumer_drain_opened: AtomicBool::new(false),
                watcher_orchestrator_opened: AtomicBool::new(false),
                poll: Arc::new(ScopedObservationPollRuntime::default()),
                ready: Arc::new(ScopedObservationReadyCompletion::default()),
            }),
        })
    }

    pub fn compatibility(&self) -> &CompatibilityDecision {
        &self.compatibility
    }

    /// Exact pre-access contract selection reported by the future portable
    /// `capabilities()` surface. Keeping it on the attachment prevents later
    /// source or delivery code from reconstructing a different selection.
    pub fn capabilities(&self) -> &ObservationContractSelection {
        &self.observation_contract
    }

    pub fn root_identity(&self) -> &ScopedObservationRootIdentity {
        &self.root_identity
    }

    #[cfg(test)]
    pub fn envelope_mapper(&self) -> ScopedObservationEnvelopeMapper {
        ScopedObservationEnvelopeMapper::new(self.root_identity.clone())
    }

    /// Construct the attachment's sole consumer-ready event drain together with
    /// its empty bounded delivery lane. The observer installs this owner before
    /// bootstrap work begins, so no envelope can cross a different or late-
    /// claimed delivery boundary.
    pub fn open_consumer_drain(
        &self,
        limits: ScopedObservationDeliveryLimits,
    ) -> Result<ScopedObservationConsumerDrain, ScopedObservationOpenDrainError> {
        if self.state.closed.load(Ordering::Acquire) || self.lifecycle.is_closing() {
            return Err(ScopedObservationOpenDrainError::Closed);
        }
        let mut drain = ScopedObservationConsumerDrain::new(
            ScopedObservationEnvelopeMapper::new(self.root_identity.clone()),
            Arc::clone(&self.attachment_authority),
            Arc::clone(&self.lifecycle),
            limits,
        )
        .map_err(ScopedObservationOpenDrainError::Delivery)?;
        self.state
            .consumer_drain_opened
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ScopedObservationOpenDrainError::AlreadyOpened)?;
        if self
            .lifecycle
            .open_consumer_drain(&drain.delivery.event_completion)
            .is_err()
        {
            return Err(ScopedObservationOpenDrainError::Closed);
        }
        drain.lifecycle_registered = true;
        if self.state.closed.load(Ordering::Acquire) || self.lifecycle.is_closing() {
            return Err(ScopedObservationOpenDrainError::Closed);
        }
        Ok(drain)
    }

    fn owns_consumer_drain(&self, drain: &ScopedObservationConsumerDrain) -> bool {
        Arc::ptr_eq(&self.attachment_authority, &drain.attachment_authority)
    }

    fn start_attachment_operation(
        &self,
    ) -> Result<ScopedObservationOperationGuard, ScopedObservationAccessError> {
        match self
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
        {
            Ok(operation) => Ok(operation),
            Err(ScopedObservationOperationStartError::Closing) => {
                Err(ScopedObservationAccessError::Closed)
            }
            Err(ScopedObservationOperationStartError::CapacityExhausted) => {
                Err(ScopedObservationAccessError::OperationCapacityExhausted)
            }
        }
    }

    /// Register one attachment-owned watcher task before it begins producing
    /// callbacks. The returned non-cloneable registration is both the task's
    /// sticky cancellation token and its close-barrier acknowledgement lease.
    pub fn register_watcher_task(
        &self,
    ) -> Result<ScopedObservationWatcherRegistration, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        let operation = match self
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Watcher)
        {
            Ok(operation) => operation,
            Err(ScopedObservationOperationStartError::Closing) => {
                return Err(ScopedObservationAccessError::Closed);
            }
            Err(ScopedObservationOperationStartError::CapacityExhausted) => {
                return Err(ScopedObservationAccessError::OperationCapacityExhausted);
            }
        };
        Ok(ScopedObservationWatcherRegistration {
            lifecycle: Arc::clone(&self.lifecycle),
            _operation: operation,
        })
    }

    /// Prepare the attachment-owned callback sink before installing a native
    /// watcher. Callers wire callbacks to the returned coordinator, install
    /// the backend, then explicitly confirm installation. The coordinator can
    /// buffer callbacks that fire synchronously during backend setup, but it
    /// cannot reserve the initial source scan until confirmation succeeds.
    pub fn prepare_watcher_install(
        &self,
        hint_capacity: usize,
    ) -> Result<ScopedObservationWatcherOrchestrator, ScopedObservationStartupError> {
        if self.state.closed.load(Ordering::Acquire) || self.lifecycle.is_closing() {
            return Err(ScopedObservationStartupError::Closed);
        }
        let mut ordering = WatchBeforeScan::new(
            self.root_identity.source_instance_key.as_bytes().to_vec(),
            hint_capacity,
        )
        .map_err(ScopedObservationStartupError::Ordering)?;
        ordering
            .watcher_registered()
            .map_err(ScopedObservationStartupError::Ordering)?;
        self.state
            .watcher_orchestrator_opened
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ScopedObservationStartupError::WatcherAlreadyInstalled)?;
        let registration = match self.register_watcher_task() {
            Ok(registration) => registration,
            Err(error) => {
                self.state
                    .watcher_orchestrator_opened
                    .store(false, Ordering::Release);
                return Err(error.into());
            }
        };
        if self.state.closed.load(Ordering::Acquire) || self.lifecycle.is_closing() {
            self.state
                .watcher_orchestrator_opened
                .store(false, Ordering::Release);
            return Err(ScopedObservationStartupError::Closed);
        }
        Ok(ScopedObservationWatcherOrchestrator {
            attachment_authority: Arc::clone(&self.attachment_authority),
            host_state: Arc::clone(&self.state),
            startup: Arc::new(Mutex::new(ScopedObservationWatcherStartupState {
                ordering,
                backend_installed: false,
                reconcile_pass_active: false,
            })),
            registration,
        })
    }

    /// Advance one admitted frame through the attachment-owned projection and
    /// delivery lane. This is the producer half of the future async facade;
    /// an equally rooted drain from another attachment cannot be substituted.
    pub fn offer_consumer_next(
        &self,
        admission: &mut ScopedObservationAdmissionLane,
        projection: &mut ScopedObservationProjectionSink,
        drain: &mut ScopedObservationConsumerDrain,
    ) -> Result<Option<ScopedObservationOfferReceipt>, ScopedObservationConsumerOfferError> {
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedObservationConsumerOfferError::ForeignDrain);
        }
        if drain.is_closed()
            || self.state.closed.load(Ordering::Acquire)
            || self.lifecycle.is_closing()
        {
            return Err(ScopedObservationConsumerOfferError::Closed);
        }
        let _operation = match self
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
        {
            Ok(operation) => operation,
            Err(ScopedObservationOperationStartError::Closing) => {
                return Err(ScopedObservationConsumerOfferError::Closed);
            }
            Err(ScopedObservationOperationStartError::CapacityExhausted) => {
                return Err(ScopedObservationConsumerOfferError::OperationCapacityExhausted);
            }
        };
        admission
            .offer_next(projection, drain.delivery_lane_mut())
            .map_err(Into::into)
    }

    fn offer_consumer_object_error(
        &self,
        drain: &mut ScopedObservationConsumerDrain,
        object: &ScopedKnownAppendObject,
        error: Arc<ScopedSourceObjectError>,
        observed_at: i64,
    ) -> Result<ScopedObservationOfferReceipt, ScopedObservationConsumerOfferError> {
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedObservationConsumerOfferError::ForeignDrain);
        }
        if drain.is_closed()
            || self.state.closed.load(Ordering::Acquire)
            || self.lifecycle.is_closing()
        {
            return Err(ScopedObservationConsumerOfferError::Closed);
        }
        if !error.validate()
            || !source_belongs_to_root(&error.source, &self.root_identity)
            || error.scope_epoch != drain.delivery_lane().state().scope_epoch
            || object.source != error.source
            || object.relation_id.as_deref() != Some(error.relation_id.as_ref())
            || !self.known_objects.contains_key(error.relation_id.as_ref())
        {
            return Err(ScopedObservationConsumerOfferError::Offer(
                ScopedProjectionDeliveryError::Projection(
                    ScopedProjectionError::ProvenanceMismatch,
                ),
            ));
        }
        let _operation = match self
            .lifecycle
            .start_operation(ScopedObservationOperationKind::Runtime)
        {
            Ok(operation) => operation,
            Err(ScopedObservationOperationStartError::Closing) => {
                return Err(ScopedObservationConsumerOfferError::Closed);
            }
            Err(ScopedObservationOperationStartError::CapacityExhausted) => {
                return Err(ScopedObservationConsumerOfferError::OperationCapacityExhausted);
            }
        };
        let event_id = source_object_error_event_id(&error);
        drain
            .delivery_lane_mut()
            .offer_projected(vec![ScopedProjectedObservation::SourceObjectError {
                source: error.source.clone(),
                observed_at,
                event_id,
                error,
            }])
            .map_err(|failure| {
                ScopedObservationConsumerOfferError::Offer(ScopedProjectionDeliveryError::Delivery(
                    failure.error,
                ))
            })
    }

    /// Admit one logical poll request. Every request receives its own ticket,
    /// while all tickets admitted before the next pass reservation share that
    /// pass and offered watermark.
    pub fn request_poll(&self) -> Result<ScopedObservationPollTicket, ScopedObservationPollError> {
        if self.state.closed.load(Ordering::Acquire) || self.lifecycle.is_closing() {
            return Err(ScopedObservationPollError::Closed);
        }
        self.state.poll.request()
    }

    /// Begin the next requested poll pass. `None` means either no request is
    /// pending or another caller already owns the coalesced in-flight pass.
    /// Requests admitted after this lease is reserved remain pending for a
    /// follow-up pass rather than being falsely acknowledged by an older
    /// source watermark.
    pub fn begin_poll(
        &self,
    ) -> Result<Option<ScopedObservationPollLease>, ScopedObservationPollError> {
        let Some(active) = self.state.poll.reserve()? else {
            return Ok(None);
        };
        let access_pass = match self.begin_pass() {
            Ok(pass) => pass,
            Err(error) => {
                self.state
                    .poll
                    .abandon(active.lease_id, active.target_generation);
                return Err(ScopedObservationPollError::Access(error));
            }
        };
        Ok(Some(ScopedObservationPollLease {
            runtime: Arc::clone(&self.state.poll),
            lease_id: active.lease_id,
            target_generation: active.target_generation,
            access_pass: Some(access_pass),
            completed: false,
        }))
    }

    pub fn poll_resolution(
        &self,
        ticket: &ScopedObservationPollTicket,
    ) -> Result<ScopedObservationPollResolution, ScopedObservationPollError> {
        self.state.poll.resolution(ticket)
    }

    pub fn poll_state(&self) -> ScopedObservationPollState {
        self.state.poll.snapshot()
    }

    /// Create a cloneable attachment-level readiness waiter without starting
    /// delivery. The future async facade starts the sole consumer drain first,
    /// then bridges this wakeable handle onto its portable runtime.
    pub fn ready_waiter(&self) -> Result<ScopedObservationReadyWaiter, ScopedObservationPollError> {
        if self.state.closed.load(Ordering::Acquire) || self.lifecycle.is_closing() {
            return Err(ScopedObservationPollError::Closed);
        }
        Ok(ScopedObservationReadyWaiter {
            completion: Arc::clone(&self.state.ready),
        })
    }

    fn validate_poll_completion(
        &self,
        lease: &ScopedObservationPollLease,
        admission: &ScopedObservationAdmissionLane,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<(), ScopedObservationPollError> {
        if !Arc::ptr_eq(&self.state.poll, &lease.runtime) {
            return Err(ScopedObservationPollError::ForeignLease);
        }
        self.validate_scope_pass_completion(lease.access_pass(), admission, drain)
    }

    fn validate_scope_pass_completion(
        &self,
        pass: &ScopedObservationAccessPass,
        admission: &ScopedObservationAdmissionLane,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<(), ScopedObservationPollError> {
        if !Arc::ptr_eq(&self.state, &pass.state) {
            return Err(ScopedObservationPollError::ForeignLease);
        }
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedObservationPollError::ForeignDrain);
        }
        if drain.is_closed() || self.lifecycle.is_closing() {
            return Err(ScopedObservationPollError::Closed);
        }
        let report = pass.report();
        let pass_id = pass.pass_id();
        if self.known_objects.keys().any(|relation_id| {
            let attempted = report
                .relations()
                .iter()
                .any(|relation| relation.relation_id == *relation_id && relation.attempts > 0);
            match admission.relation_pass_evidence(relation_id, pass_id) {
                Some(ScopedCoveragePassEvidence::AccessAttempt) => !attempted,
                Some(ScopedCoveragePassEvidence::RetainedObjectError) => attempted,
                None => true,
            }
        }) {
            return Err(ScopedObservationPollError::IncompleteScopePass);
        }
        Ok(())
    }

    fn validate_epoch_poll_execution(
        &self,
        lease: &ScopedObservationPollLease,
        active: &ScopedObservationEpochState,
        drain: &ScopedObservationConsumerDrain,
        requests: &[ScopedObservationAppendPassRequest<'_>],
    ) -> Result<(), ScopedObservationPassExecutionError> {
        if !Arc::ptr_eq(&self.state.poll, &lease.runtime)
            || !Arc::ptr_eq(&self.state, &lease.access_pass().state)
        {
            return Err(ScopedObservationPollError::ForeignLease.into());
        }
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedObservationPollError::ForeignDrain.into());
        }
        if drain.is_closed()
            || self.state.closed.load(Ordering::Acquire)
            || self.lifecycle.is_closing()
        {
            return Err(ScopedObservationPollError::Closed.into());
        }
        let queue_state = drain.delivery_lane().state();
        if !Arc::ptr_eq(&active.attachment_authority, &self.attachment_authority)
            || active.root != self.root_identity
            || active.scope_epoch != queue_state.scope_epoch
            || active.projection.lifecycle != ScopedProjectionLifecycle::Active
            || queue_state.continuity != ScopedObservationContinuity::Valid
            || active.append_objects.len() != self.known_objects.len()
            || active
                .append_objects
                .keys()
                .any(|relation_id| !self.known_objects.contains_key(relation_id))
            || !scoped_object_errors_match_epoch(active)
        {
            return Err(ScopedObservationPassExecutionError::InvalidEpochState);
        }

        if requests.len() != active.append_objects.len() {
            return Err(ScopedObservationPassExecutionError::InvalidRelationSet);
        }
        let mut relation_ids = BTreeMap::new();
        for request in requests {
            if relation_ids.insert(request.relation_id, request).is_some()
                || !active.append_objects.contains_key(request.relation_id)
            {
                return Err(ScopedObservationPassExecutionError::InvalidRelationSet);
            }
        }
        if active
            .append_objects
            .keys()
            .any(|relation_id| !relation_ids.contains_key(relation_id.as_str()))
        {
            return Err(ScopedObservationPassExecutionError::InvalidRelationSet);
        }
        Ok(())
    }

    fn validate_epoch_source_owner_binding(
        &self,
        active: &ScopedObservationEpochState,
        drain: &ScopedObservationConsumerDrain,
        bindings: &[ScopedObservationAppendPassBinding],
        policy: ScopedObservationSourceOwnerRetryPolicy,
    ) -> Result<(), ScopedObservationSourceOwnerBindingError> {
        if !self.owns_consumer_drain(drain)
            || !Arc::ptr_eq(&active.attachment_authority, &self.attachment_authority)
        {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidEpochState);
        }
        if drain.is_closed()
            || self.state.closed.load(Ordering::Acquire)
            || self.lifecycle.is_closing()
        {
            return Err(ScopedObservationSourceOwnerBindingError::Closed);
        }
        let queue_state = drain.delivery_lane().state();
        if active.root != self.root_identity
            || active.scope_epoch != queue_state.scope_epoch
            || active.projection.lifecycle != ScopedProjectionLifecycle::Active
            || queue_state.continuity != ScopedObservationContinuity::Valid
            || active.append_objects.len() != self.known_objects.len()
            || active
                .append_objects
                .keys()
                .any(|relation_id| !self.known_objects.contains_key(relation_id))
            || !scoped_object_errors_match_epoch(active)
        {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidEpochState);
        }
        if active
            .object_errors
            .values()
            .any(|state| !policy.accepts_retained_retry(state.error.retry))
        {
            return Err(ScopedObservationSourceOwnerBindingError::RetryPolicyMismatch);
        }
        if bindings.len() != active.append_objects.len() {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidRelationSet);
        }
        let program = self
            .authorization
            .select_scope_program(&self.program_id)
            .map_err(|_| ScopedObservationSourceOwnerBindingError::AuthorizedProgramUnavailable)?;
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program)
            .map_err(|_| ScopedObservationSourceOwnerBindingError::AuthorizedProgramUnavailable)?;
        let mut relation_ids = BTreeMap::new();
        for binding in bindings {
            let object = active
                .append_objects
                .get(binding.relation_id.as_str())
                .ok_or(ScopedObservationSourceOwnerBindingError::InvalidRelationSet)?;
            if relation_ids
                .insert(binding.relation_id.as_str(), ())
                .is_some()
            {
                return Err(ScopedObservationSourceOwnerBindingError::InvalidRelationSet);
            }
            let declaration = plan
                .relation(&binding.relation_id)
                .ok_or(ScopedObservationSourceOwnerBindingError::InvalidRelationSet)?;
            if declaration.identity_inputs.len() != binding.identity_inputs.len()
                || declaration
                    .identity_inputs
                    .iter()
                    .zip(&binding.identity_inputs)
                    .any(|(expected, actual)| expected != &actual.name)
            {
                return Err(ScopedObservationSourceOwnerBindingError::InvalidIdentityInput);
            }
            if binding.depth > declaration.bounds.max_depth
                || binding.max_bytes > declaration.bounds.max_bytes
            {
                return Err(ScopedObservationSourceOwnerBindingError::InvalidBounds);
            }
            let mut token_components = Vec::with_capacity(binding.identity_inputs.len() * 2);
            for input in &binding.identity_inputs {
                token_components.push(input.name.as_bytes());
                token_components.push(input.value.as_slice());
            }
            let object_token =
                AccessObjectToken::derive(&binding.relation_id, &token_components)
                    .map_err(|_| ScopedObservationSourceOwnerBindingError::InvalidIdentityInput)?;
            let expected_access_identity = ScopedAppendAccessIdentity {
                object_token,
                parent_token: binding.parent_token,
                depth: binding.depth,
                source_instance_id: binding.origin.source_instance_id,
                stream_id: binding.origin.stream_id,
                object_id: binding.origin.object_id,
                media_type: binding.origin.media_type.clone(),
            };
            if object.access_identity.as_ref() != Some(&expected_access_identity) {
                return Err(ScopedObservationSourceOwnerBindingError::AccessIdentityMismatch);
            }
        }
        if active
            .append_objects
            .keys()
            .any(|relation_id| !relation_ids.contains_key(relation_id.as_str()))
        {
            return Err(ScopedObservationSourceOwnerBindingError::InvalidRelationSet);
        }
        Ok(())
    }

    fn offer_epoch_poll_pending(
        &self,
        active: &mut ScopedObservationEpochState,
        drain: &mut ScopedObservationConsumerDrain,
    ) -> Result<(), ScopedObservationPassExecutionError> {
        loop {
            let offered =
                self.offer_consumer_next(&mut active.admission, &mut active.projection, drain)?;
            if offered.is_none() {
                return Ok(());
            }
        }
    }

    fn reconcile_epoch_poll_relation(
        &self,
        lease: &ScopedObservationPollLease,
        active: &mut ScopedObservationEpochState,
        request: &ScopedObservationAppendPassRequest<'_>,
    ) -> Result<ScopedObservationRelationPollOutcome, ScopedObservationPassExecutionError> {
        let relation_id = request.relation_id;
        let observation = {
            let object = active
                .append_objects
                .get_mut(relation_id)
                .expect("the exact relation set was prevalidated");
            object.reconcile(
                lease.access_pass(),
                ScopedAppendReconcileRequest {
                    relation_id,
                    identity_inputs: request.identity_inputs,
                    access_phase: AccessPhase::Revalidation,
                    parent_token: request.parent_token,
                    depth: request.depth,
                    max_bytes: request.max_bytes,
                    origin: request.origin,
                    force_contract_replay: request.force_contract_replay,
                },
            )?
        };

        let decoded = {
            let object = active
                .append_objects
                .get_mut(relation_id)
                .expect("the exact relation set was prevalidated");
            match self.decode_append(object, &observation) {
                Ok(ScopedAppendDecodeOutcome::Ready(decoded)) => decoded,
                Ok(ScopedAppendDecodeOutcome::RetryTransient) => {
                    object.discard(&observation)?;
                    return Err(ScopedObservationPassExecutionError::DecodeRetryTransient);
                }
                Err(error) => {
                    object.discard(&observation)?;
                    return Err(error.into());
                }
            }
        };
        let outcome = if matches!(&observation.read, AppendRead::RetryTransient) {
            ScopedObservationRelationPollOutcome::RetryTransient
        } else {
            ScopedObservationRelationPollOutcome::Ready
        };

        let admission_result = {
            let object = active
                .append_objects
                .get_mut(relation_id)
                .expect("the exact relation set was prevalidated");
            active.admission.admit(object, &observation, decoded)
        };
        if let Err(failure) = admission_result {
            active
                .append_objects
                .get_mut(relation_id)
                .expect("the exact relation set was prevalidated")
                .discard(&observation)?;
            return Err(ScopedObservationPassExecutionError::Admission(
                failure.error,
            ));
        }
        Ok(outcome)
    }

    /// Execute one complete live exact-scope append pass for a reserved poll.
    ///
    /// Requests are prevalidated as an exact set and then visited in canonical
    /// relation order. Any admission or offered-boundary failure drops the
    /// unfinished lease, so its logical poll tickets remain pending and the
    /// same target becomes runnable again. Source cursor/decoder state already
    /// committed by admission remains paired with its queued frame; a retry
    /// flushes that frame before taking fresh bounded reads. Poll completion is
    /// the final operation and therefore cannot acknowledge coverage from an
    /// older pass. Async attachment owners must use the handle-level method so
    /// native access never runs while their consumer-drain mutex is held.
    pub fn execute_epoch_poll_pass(
        &self,
        lease: ScopedObservationPollLease,
        active: &mut ScopedObservationEpochState,
        drain: &mut ScopedObservationConsumerDrain,
        requests: &[ScopedObservationAppendPassRequest<'_>],
    ) -> Result<Arc<ScopedObservationWatermarkCore>, ScopedObservationPassExecutionError> {
        self.validate_epoch_poll_execution(&lease, active, drain, requests)?;

        // A prior retry may have committed cursor state only as far as the
        // bounded admission lane. Offer it before new access so capacity and
        // source ordering remain bounded and deterministic.
        self.offer_epoch_poll_pending(active, drain)?;

        let requests_by_relation = requests
            .iter()
            .map(|request| (request.relation_id, request))
            .collect::<BTreeMap<_, _>>();
        let relation_ids = active.append_objects.keys().cloned().collect::<Vec<_>>();
        for relation_id in relation_ids {
            let request = requests_by_relation
                .get(relation_id.as_str())
                .expect("the exact relation set was prevalidated");
            self.reconcile_epoch_poll_relation(&lease, active, request)?;
            self.offer_epoch_poll_pending(active, drain)?;
        }

        self.complete_epoch_poll(lease, active, drain)
            .map_err(Into::into)
    }

    /// Complete a bootstrap-era poll only after its entire exact known-object
    /// pass is represented by the offered coverage boundary.
    pub fn complete_bootstrap_poll(
        &self,
        lease: ScopedObservationPollLease,
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<Arc<ScopedObservationWatermarkCore>, ScopedObservationPollError> {
        self.validate_poll_completion(&lease, admission, drain)?;
        let watermark =
            self.capture_watermark_core(admission, projection, drain.delivery_lane())?;
        lease.complete(watermark)
    }

    /// Complete a live/correction poll against the one active epoch owner.
    /// Coverage cannot outrun delivery because the captured watermark uses the
    /// drain's attachment-owned offered boundary.
    pub fn complete_epoch_poll(
        &self,
        lease: ScopedObservationPollLease,
        active: &ScopedObservationEpochState,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<Arc<ScopedObservationWatermarkCore>, ScopedObservationPollError> {
        self.validate_poll_completion(&lease, &active.admission, drain)?;
        let watermark = self.capture_epoch_watermark(active, drain.delivery_lane())?;
        lease.complete(watermark)
    }

    /// Internal readiness probe for the future async facade. It observes only
    /// the retained engine-offered barrier; consumer-applied readiness remains
    /// on `ScopedObservationConsumerDrain`.
    pub fn engine_ready(
        &self,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<Option<Arc<ScopedBootstrapBarrier>>, ScopedObservationPollError> {
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedObservationPollError::ForeignDrain);
        }
        if drain.is_closed()
            || self.state.closed.load(Ordering::Acquire)
            || self.lifecycle.is_closing()
        {
            return Err(ScopedObservationPollError::Closed);
        }
        match self.state.ready.snapshot() {
            ScopedObservationReadyResolution::Pending => {
                debug_assert!(drain.engine_bootstrap_barrier().is_none());
                Ok(None)
            }
            ScopedObservationReadyResolution::Ready(barrier) => {
                let drain_barrier = drain
                    .engine_bootstrap_barrier()
                    .ok_or(ScopedObservationPollError::ReadinessStateMismatch)?;
                if !Arc::ptr_eq(&barrier, &drain_barrier) {
                    return Err(ScopedObservationPollError::ReadinessStateMismatch);
                }
                Ok(Some(barrier))
            }
            ScopedObservationReadyResolution::Failed(_) => {
                Err(ScopedObservationPollError::ObserverFailed)
            }
            ScopedObservationReadyResolution::Cancelled => Err(ScopedObservationPollError::Closed),
        }
    }

    /// Capture a self-consistent offered sequence plus eligible RFC 012A
    /// source/fact-family coverage after the admission lane has drained and
    /// every exact host-authorized known-object relation has contributed one
    /// admitted coverage member. A known missing object counts through its
    /// explicit absence; a relation that was never reconciled does not.
    /// Delivery backlog is allowed: offered and delivered are deliberately
    /// different boundaries.
    pub fn capture_watermark_core(
        &self,
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<ScopedObservationWatermarkCore, ScopedCoverageAssemblyError> {
        let queue_state = delivery.state();
        if matches!(
            queue_state.continuity,
            ScopedObservationContinuity::ResyncRequired | ScopedObservationContinuity::Failed
        ) {
            return Err(ScopedCoverageAssemblyError::ContinuityInvalid);
        }
        validate_scoped_relation_coverage(self.known_objects.as_ref(), admission)?;
        let source_coverage = assemble_scoped_coverage_sets(
            self.adapter.manifest(),
            &self.observation_contract.contract_versions,
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
        append_objects: &[ScopedKnownAppendObject],
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        delivery: &mut ScopedObservationDeliveryLane,
        observed_at: i64,
    ) -> Result<Arc<ScopedBootstrapBarrier>, ScopedBootstrapBarrierError> {
        if matches!(
            delivery.state().continuity,
            ScopedObservationContinuity::ResyncRequired
                | ScopedObservationContinuity::Resyncing
                | ScopedObservationContinuity::Failed
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
        let root_present = validate_bootstrap_source_state(
            self.known_objects.as_ref(),
            &self.root_relation_id,
            &self.root_identity,
            admission,
            append_objects,
        )?;
        let replacement = projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Bootstrap)
            .map_err(|_| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        let family_manifest = replacement_family_manifest(&replacement, &watermark.source_coverage)
            .map_err(|_| ScopedBootstrapBarrierError::InvalidSnapshot)?;
        delivery.offer_bootstrap_barrier(
            &self.root_identity,
            watermark,
            family_manifest,
            root_present,
            observed_at,
        )
    }

    /// Producer entry point for the attachment-owned consumer drain. This
    /// prevents an equally rooted second observer from substituting its queue
    /// at the bootstrap/ready boundary.
    pub fn offer_consumer_bootstrap_complete(
        &self,
        append_objects: &[ScopedKnownAppendObject],
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        drain: &mut ScopedObservationConsumerDrain,
        observed_at: i64,
    ) -> Result<Arc<ScopedBootstrapBarrier>, ScopedBootstrapBarrierError> {
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedBootstrapBarrierError::ForeignDrain);
        }
        if drain.is_closed() || self.lifecycle.is_closing() {
            return Err(ScopedBootstrapBarrierError::Closed);
        }
        let barrier = self.offer_bootstrap_complete(
            append_objects,
            admission,
            projection,
            drain.delivery_lane_mut(),
            observed_at,
        )?;
        self.state.ready.complete(Arc::clone(&barrier));
        Ok(barrier)
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

    /// Bind the fully offered bootstrap components into one active epoch owner
    /// before resync is possible. This is the first composition that prevents
    /// source, coverage, and reducer state from being swapped independently.
    pub fn bind_bootstrap_epoch_state(
        &self,
        append_objects: Vec<ScopedKnownAppendObject>,
        admission: ScopedObservationAdmissionLane,
        projection: ScopedObservationProjectionSink,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<ScopedObservationEpochState, ScopedReplacementStageError> {
        let barrier = delivery
            .bootstrap_barrier()
            .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
        if barrier.root != self.root_identity
            || barrier.scope_epoch != SCOPED_INITIAL_SCOPE_EPOCH
            || delivery.state().scope_epoch != SCOPED_INITIAL_SCOPE_EPOCH
            || delivery.state().continuity != ScopedObservationContinuity::Valid
            || projection.lifecycle != ScopedProjectionLifecycle::Active
        {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        let watermark = self
            .capture_watermark_core(&admission, &projection, delivery)
            .map_err(ScopedReplacementStageError::Coverage)?;
        if watermark.source_coverage != barrier.source_coverage
            || watermark.explicit_object_errors != barrier.explicit_object_errors
        {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        let replacement = projection
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Bootstrap)
            .map_err(ScopedReplacementStageError::Projection)?;
        let family_manifest =
            replacement_family_manifest(&replacement, &watermark.source_coverage)?;
        let replacement_digest = replacement_snapshot_digest(
            &self.root_identity,
            barrier.root_present,
            &family_manifest,
            &watermark.source_coverage,
            &watermark.explicit_object_errors,
        )?;
        if family_manifest != barrier.family_manifest
            || replacement_digest != barrier.replacement_snapshot_digest
        {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        let append_objects = bind_active_scoped_append_objects(
            self.known_objects.as_ref(),
            &self.root_identity,
            &admission,
            append_objects,
        )?;
        if append_objects
            .get(self.root_relation_id.as_ref())
            .is_none_or(|object| object.is_present() != barrier.root_present)
        {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        Ok(ScopedObservationEpochState {
            attachment_authority: Arc::clone(&self.attachment_authority),
            root: self.root_identity.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            append_objects,
            admission,
            projection,
            object_errors: BTreeMap::new(),
        })
    }

    /// Bind epoch 1 using the exact queue owner created by this attachment.
    pub fn bind_consumer_bootstrap_epoch_state(
        &self,
        append_objects: Vec<ScopedKnownAppendObject>,
        admission: ScopedObservationAdmissionLane,
        projection: ScopedObservationProjectionSink,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<ScopedObservationEpochState, ScopedReplacementStageError> {
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        if drain.is_closed() || self.lifecycle.is_closing() {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        self.bind_bootstrap_epoch_state(
            append_objects,
            admission,
            projection,
            drain.delivery_lane(),
        )
    }

    /// Capture the active epoch after a whole-scope activation without
    /// allowing callers to pair one epoch's source/coverage with another
    /// projection sink.
    pub fn capture_epoch_watermark(
        &self,
        active: &ScopedObservationEpochState,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<ScopedObservationWatermarkCore, ScopedCoverageAssemblyError> {
        if !Arc::ptr_eq(&active.attachment_authority, &self.attachment_authority)
            || active.root != self.root_identity
            || active.scope_epoch != delivery.state().scope_epoch
            || active.projection.lifecycle != ScopedProjectionLifecycle::Active
        {
            return Err(ScopedCoverageAssemblyError::ContinuityInvalid);
        }
        self.capture_watermark_core(&active.admission, &active.projection, delivery)
    }

    /// Freeze every active append object and create empty source, coverage,
    /// and reducer state for the current replacement epoch. A newer re-overflow
    /// may supersede the lineage links; a partial construction restores them.
    pub fn open_scope_resync_stage(
        &self,
        active: &mut ScopedObservationEpochState,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<ScopedObservationScopeReplacementStage, ScopedReplacementStageError> {
        let started = delivery
            .resync_started
            .as_ref()
            .ok_or(ScopedReplacementStageError::NotResyncing)?;
        let baseline_scope_epoch = valid_replacement_baseline_scope_epoch(delivery)
            .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
        if !Arc::ptr_eq(&active.attachment_authority, &self.attachment_authority)
            || active.root != self.root_identity
            || started.root != self.root_identity
            || active.scope_epoch != baseline_scope_epoch
            || delivery.state().scope_epoch != started.new_scope_epoch
            || active.projection.lifecycle != ScopedProjectionLifecycle::Active
            || active.append_objects.len() != self.known_objects.len()
            || active
                .append_objects
                .keys()
                .any(|relation_id| !self.known_objects.contains_key(relation_id))
        {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        let semantic = self.open_projection_resync_stage(&active.projection, delivery)?;
        let admission = ScopedObservationAdmissionLane::new(active.admission.limits)
            .map_err(|_| ScopedReplacementStageError::InvalidSourceState)?;
        let prior_lifecycles = active
            .append_objects
            .iter()
            .map(|(relation_id, object)| (relation_id.clone(), object.lifecycle))
            .collect::<Vec<_>>();
        let mut append_objects = BTreeMap::new();
        for (relation_id, object) in &mut active.append_objects {
            let replacement = match object.fork_replacement(started.new_scope_epoch) {
                Ok(replacement) => replacement,
                Err(_) => {
                    for (prior_relation_id, prior_lifecycle) in prior_lifecycles {
                        active
                            .append_objects
                            .get_mut(&prior_relation_id)
                            .expect("saved scoped append relation remains present")
                            .lifecycle = prior_lifecycle;
                    }
                    return Err(ScopedReplacementStageError::InvalidSourceState);
                }
            };
            append_objects.insert(relation_id.clone(), replacement);
        }
        Ok(ScopedObservationScopeReplacementStage {
            semantic,
            append_objects,
            admission,
            activated: false,
        })
    }

    /// Allocate the isolated empty reducer for the current replacement epoch.
    /// The active reducer supplies only its fixed capacity policy; none of its
    /// semantic state is cloned into replacement.
    fn open_projection_resync_stage(
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

    /// Projection-only conformance seam. Production composition must use
    /// `open_scope_resync_stage` so source and coverage state cannot be left
    /// outside the replacement owner.
    #[cfg(test)]
    pub fn open_resync_stage(
        &self,
        active: &ScopedObservationProjectionSink,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<ScopedObservationReplacementStage, ScopedReplacementStageError> {
        self.open_projection_resync_stage(active, delivery)
    }

    /// Validate the frozen per-family replacement against the exact offered
    /// coverage watermark, enqueue completion after every snapshot entity,
    /// then infallibly swap the isolated reducer into the active slot.
    fn offer_projection_resync_complete(
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

    /// Projection-only conformance seam. Production completion must use
    /// `offer_scope_resync_complete` for one source/coverage/reducer transfer.
    #[cfg(test)]
    pub fn offer_resync_complete(
        &self,
        active: &mut ScopedObservationProjectionSink,
        stage: &mut ScopedObservationReplacementStage,
        admission: &ScopedObservationAdmissionLane,
        delivery: &mut ScopedObservationDeliveryLane,
        root_present: bool,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncBarrier>, ScopedReplacementStageError> {
        self.offer_projection_resync_complete(
            active,
            stage,
            admission,
            delivery,
            root_present,
            observed_at,
        )
    }

    /// Offer one replacement barrier and then infallibly transfer its append
    /// state, offered coverage lane, and semantic reducers into the active
    /// epoch. Every fallible validation occurs before the control offer.
    pub fn offer_scope_resync_complete(
        &self,
        active: &mut ScopedObservationEpochState,
        stage: &mut ScopedObservationScopeReplacementStage,
        delivery: &mut ScopedObservationDeliveryLane,
        observed_at: i64,
    ) -> Result<Arc<ScopedResyncBarrier>, ScopedReplacementStageError> {
        if !Arc::ptr_eq(&active.attachment_authority, &self.attachment_authority) {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        if stage.activated {
            let barrier = stage
                .semantic
                .completed
                .as_ref()
                .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
            return if active.root == self.root_identity
                && active.scope_epoch == barrier.scope_epoch
                && barrier.root == self.root_identity
            {
                Ok(Arc::clone(barrier))
            } else {
                Err(ScopedReplacementStageError::InvalidSourceState)
            };
        }
        validate_scope_replacement_source_state(
            self.known_objects.as_ref(),
            &self.root_identity,
            active,
            stage,
            delivery,
        )?;
        let root_present = stage
            .append_objects
            .get(self.root_relation_id.as_ref())
            .ok_or(ScopedReplacementStageError::InvalidSourceState)?
            .is_present();
        let empty_retired_admission = ScopedObservationAdmissionLane::new(active.admission.limits)
            .map_err(|_| ScopedReplacementStageError::InvalidSourceState)?;
        let barrier = self.offer_projection_resync_complete(
            &mut active.projection,
            &mut stage.semantic,
            &stage.admission,
            delivery,
            root_present,
            observed_at,
        )?;

        for (relation_id, active_object) in &mut active.append_objects {
            let replacement = stage
                .append_objects
                .get_mut(relation_id)
                .expect("prevalidated replacement relation remains present");
            active_object.activate_replacement_prevalidated(replacement);
        }
        let replacement_admission =
            std::mem::replace(&mut stage.admission, empty_retired_admission);
        let retired_admission = std::mem::replace(&mut active.admission, replacement_admission);
        drop(retired_admission);
        active.scope_epoch = barrier.scope_epoch;
        active.object_errors.clear();
        stage.activated = true;
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
        let _operation = self.start_attachment_operation()?;
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
        let operation = match self.start_attachment_operation() {
            Ok(operation) => operation,
            Err(error) => {
                self.state.pass_active.store(false, Ordering::Release);
                return Err(error);
            }
        };
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
        let pass_id = match self.state.next_pass_id.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| current.checked_add(1),
        ) {
            Ok(pass_id) => pass_id,
            Err(_) => {
                self.state.pass_active.store(false, Ordering::Release);
                return Err(ScopedObservationAccessError::AccessPassSequenceExhausted);
            }
        };
        Ok(ScopedObservationAccessPass {
            pass_id,
            plan,
            known_objects: Arc::clone(&self.known_objects),
            root_identity: self.root_identity.clone(),
            state: Arc::clone(&self.state),
            _operation: operation,
            released: false,
        })
    }

    /// Idempotently request attachment cancellation. The returned barrier is
    /// complete only after all registered work and the opened consumer drain
    /// have stopped; dropping the host initiates but deliberately does not
    /// block on that acknowledgement.
    pub fn close(&self) -> ScopedObservationCloseBarrier {
        let barrier = self.lifecycle.begin_close();
        self.state.closed.store(true, Ordering::Release);
        self.state.poll.close();
        self.state.ready.cancel();
        barrier
    }

    /// Close the exact attachment-owned drain and return the shared completion
    /// barrier. The future public observer facade owns both values and uses
    /// this path before awaiting close.
    pub fn close_with_consumer(
        &self,
        drain: &mut ScopedObservationConsumerDrain,
    ) -> Result<ScopedObservationCloseBarrier, ScopedObservationCloseError> {
        if !self.owns_consumer_drain(drain) {
            return Err(ScopedObservationCloseError::ForeignDrain);
        }
        let barrier = self.close();
        drain.close();
        Ok(barrier)
    }

    pub fn is_closed(&self) -> bool {
        self.state.closed.load(Ordering::Acquire)
    }
}

impl Drop for ScopedObservationAccessHost {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// One bounded reconciliation pass. Dropping the pass releases the host's
/// single-pass slot; a later pass receives a fresh declaration-sized ledger.
pub struct ScopedObservationAccessPass {
    pass_id: u64,
    plan: AuthorizedScopeAccessPlan,
    known_objects: Arc<BTreeMap<String, ScopedKnownObjectGrant>>,
    root_identity: ScopedObservationRootIdentity,
    state: Arc<ScopedObservationAccessState>,
    _operation: ScopedObservationOperationGuard,
    released: bool,
}

impl ScopedObservationAccessPass {
    pub fn pass_id(&self) -> u64 {
        self.pass_id
    }

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

    fn validate_known_relation(
        &self,
        relation_id: &str,
    ) -> Result<(), ScopedObservationAccessError> {
        if self.known_objects.contains_key(relation_id) {
            Ok(())
        } else {
            Err(ScopedObservationAccessError::InvalidGrant(format!(
                "relation {relation_id:?} has no exact known-object grant"
            )))
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
        expected_object_token: Option<AccessObjectToken>,
    ) -> Result<(AppendRead, AccessObjectToken), ScopedObservationAccessError> {
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
        let object_token = reservation.object_token();
        if expected_object_token.is_some_and(|expected| expected != object_token) {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::InvalidGrant(
                "known-object identity inputs changed after their first authorized access"
                    .to_string(),
            ));
        }
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
        Ok((read, object_token))
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

impl ScopedObservationWatcherOrchestrator {
    fn lock_startup(&self) -> MutexGuard<'_, ScopedObservationWatcherStartupState> {
        self.startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn validate_host(
        &self,
        host: &ScopedObservationAccessHost,
    ) -> Result<(), ScopedObservationStartupError> {
        if !Arc::ptr_eq(&self.attachment_authority, &host.attachment_authority) {
            return Err(ScopedObservationStartupError::ForeignHost);
        }
        if self.registration.cancellation_requested()
            || host.state.closed.load(Ordering::Acquire)
            || host.lifecycle.is_closing()
        {
            return Err(ScopedObservationStartupError::Closed);
        }
        Ok(())
    }

    pub fn phase(&self) -> ScopedObservationWatcherPhase {
        scoped_watcher_phase(&self.lock_startup())
    }

    pub fn cancellation_requested(&self) -> bool {
        self.registration.cancellation_requested()
    }

    pub fn wait_for_cancellation(&self) {
        self.registration.wait_for_cancellation();
    }

    pub async fn wait_for_cancellation_async(&self) {
        self.registration.wait_for_cancellation_async().await;
    }

    /// Confirm that the native backend is active after its callback has been
    /// wired to this coordinator. Repeating confirmation is idempotent; source
    /// access remains impossible until the first successful confirmation.
    pub fn confirm_watcher_installed(
        &self,
        host: &ScopedObservationAccessHost,
    ) -> Result<ScopedObservationWatcherPhase, ScopedObservationStartupError> {
        self.validate_host(host)?;
        let mut startup = self.lock_startup();
        startup.backend_installed = true;
        Ok(scoped_watcher_phase(&startup))
    }

    /// Accept one watcher callback. Before the bootstrap barrier it is kept in
    /// the bounded/coalescing startup set. After the barrier it atomically
    /// becomes a request-local poll ticket instead of racing bootstrap offer.
    pub fn record_hint(
        &self,
        host: &ScopedObservationAccessHost,
        hint: DirtyHint,
    ) -> Result<ScopedObservationWatcherHintAction, ScopedObservationStartupError> {
        self.validate_host(host)?;
        let mut startup = self.lock_startup();
        let action = startup
            .ordering
            .push_hint(hint)
            .map_err(ScopedObservationStartupError::Ordering)?;
        match action {
            StartupAction::Buffered(enqueue) => {
                Ok(ScopedObservationWatcherHintAction::Buffered(enqueue))
            }
            StartupAction::DeliverNow(hint) => {
                drop(startup);
                let ticket = host.request_poll()?;
                Ok(ScopedObservationWatcherHintAction::PollRequested { hint, ticket })
            }
            StartupAction::Reconcile(_)
            | StartupAction::AwaitMoreReconcile
            | StartupAction::Live { .. } => {
                let phase = scoped_watcher_phase(&startup);
                Err(ScopedObservationStartupError::InvalidPhase {
                    operation: "record watcher hint",
                    phase,
                })
            }
        }
    }

    /// Reserve the first exact-scope pass only after the watcher backend has
    /// been confirmed installed. An unfinished/dropped scan is retryable and
    /// preserves every callback buffered during the attempt.
    pub fn begin_initial_scan(
        &self,
        host: &ScopedObservationAccessHost,
    ) -> Result<ScopedObservationInitialScan, ScopedObservationStartupError> {
        self.validate_host(host)?;
        let mut startup = self.lock_startup();
        if !startup.backend_installed || startup.ordering.phase() != StartupPhase::WatchRegistered {
            return Err(ScopedObservationStartupError::InvalidPhase {
                operation: "begin initial scan",
                phase: scoped_watcher_phase(&startup),
            });
        }
        let access_pass = host.begin_pass()?;
        startup
            .ordering
            .begin_scan()
            .map_err(ScopedObservationStartupError::Ordering)?;
        Ok(ScopedObservationInitialScan {
            startup: Arc::clone(&self.startup),
            access_pass: Some(access_pass),
            completed: false,
        })
    }

    /// Close the initial scan only after every exact known-object relation has
    /// attempted access and published coverage from this same pass.
    pub fn finish_initial_scan(
        &self,
        host: &ScopedObservationAccessHost,
        mut scan: ScopedObservationInitialScan,
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<ScopedObservationWatermarkCore, ScopedObservationStartupError> {
        self.validate_host(host)?;
        if !Arc::ptr_eq(&self.startup, &scan.startup) {
            return Err(ScopedObservationStartupError::ForeignPass);
        }
        let mut startup = self.lock_startup();
        if startup.ordering.phase() != StartupPhase::Scanning {
            return Err(ScopedObservationStartupError::InvalidPhase {
                operation: "finish initial scan",
                phase: scoped_watcher_phase(&startup),
            });
        }
        let pass = scan
            .access_pass
            .as_ref()
            .expect("an unfinished initial scan retains its scoped access pass");
        host.validate_scope_pass_completion(pass, admission, drain)?;
        let watermark = host
            .capture_watermark_core(admission, projection, drain.delivery_lane())
            .map_err(ScopedObservationPollError::Coverage)?;
        startup
            .ordering
            .finish_scan()
            .map_err(ScopedObservationStartupError::Ordering)?;
        scan.completed = true;
        drop(scan.access_pass.take());
        Ok(watermark)
    }

    /// Drain one bounded startup hint batch and reserve a fresh whole-scope
    /// reconciliation pass. `CaughtUp` is only a provisional observation;
    /// callbacks remain buffered until `offer_bootstrap_complete` holds this
    /// same lock and successfully offers the barrier.
    pub fn next_reconcile(
        &self,
        host: &ScopedObservationAccessHost,
        max_hints: usize,
    ) -> Result<ScopedObservationStartupReconcileAction, ScopedObservationStartupError> {
        self.validate_host(host)?;
        if max_hints == 0 {
            return Err(ScopedObservationStartupError::InvalidReconcileLimit);
        }
        let mut startup = self.lock_startup();
        if !matches!(
            startup.ordering.phase(),
            StartupPhase::Replaying | StartupPhase::Reconciling
        ) {
            return Err(ScopedObservationStartupError::InvalidPhase {
                operation: "begin startup reconciliation",
                phase: scoped_watcher_phase(&startup),
            });
        }
        if startup.reconcile_pass_active {
            return Err(ScopedObservationStartupError::ReconcilePassActive);
        }
        if startup.ordering.pending_hint_count() == 0 {
            let action = startup
                .ordering
                .next_reconcile_batch(max_hints)
                .map_err(ScopedObservationStartupError::Ordering)?;
            debug_assert!(matches!(action, StartupAction::AwaitMoreReconcile));
            return Ok(ScopedObservationStartupReconcileAction::CaughtUp);
        }

        let access_pass = host.begin_pass()?;
        let action = startup
            .ordering
            .next_reconcile_batch(max_hints)
            .map_err(ScopedObservationStartupError::Ordering)?;
        let StartupAction::Reconcile(hints) = action else {
            return Err(ScopedObservationStartupError::InvalidPhase {
                operation: "reserve startup reconciliation",
                phase: scoped_watcher_phase(&startup),
            });
        };
        startup.reconcile_pass_active = true;
        Ok(ScopedObservationStartupReconcileAction::Reconcile(
            Box::new(ScopedObservationStartupReconcilePass {
                startup: Arc::clone(&self.startup),
                hints,
                access_pass: Some(access_pass),
                completed: false,
            }),
        ))
    }

    /// Publish the exact-scope offered watermark for one startup reconcile.
    /// Failure or drop restores the pass's coalesced hints automatically.
    pub fn finish_reconcile(
        &self,
        host: &ScopedObservationAccessHost,
        mut reconcile: Box<ScopedObservationStartupReconcilePass>,
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        drain: &ScopedObservationConsumerDrain,
    ) -> Result<ScopedObservationWatermarkCore, ScopedObservationStartupError> {
        self.validate_host(host)?;
        if !Arc::ptr_eq(&self.startup, &reconcile.startup) {
            return Err(ScopedObservationStartupError::ForeignPass);
        }
        let mut startup = self.lock_startup();
        if startup.ordering.phase() != StartupPhase::Reconciling || !startup.reconcile_pass_active {
            return Err(ScopedObservationStartupError::InvalidPhase {
                operation: "finish startup reconciliation",
                phase: scoped_watcher_phase(&startup),
            });
        }
        let pass = reconcile
            .access_pass
            .as_ref()
            .expect("an unfinished startup reconciliation retains its access pass");
        host.validate_scope_pass_completion(pass, admission, drain)?;
        let watermark = host
            .capture_watermark_core(admission, projection, drain.delivery_lane())
            .map_err(ScopedObservationPollError::Coverage)?;
        startup.reconcile_pass_active = false;
        reconcile.completed = true;
        drop(reconcile.access_pass.take());
        Ok(watermark)
    }

    /// Atomically offer the bootstrap barrier and release subsequent watcher
    /// callbacks into live poll scheduling. Queue pressure leaves startup in
    /// `Reconciling`; a callback cannot slip between the successful offer and
    /// the transition to `Live`.
    pub fn offer_bootstrap_complete(
        &self,
        host: &ScopedObservationAccessHost,
        append_objects: &[ScopedKnownAppendObject],
        admission: &ScopedObservationAdmissionLane,
        projection: &ScopedObservationProjectionSink,
        drain: &mut ScopedObservationConsumerDrain,
        observed_at: i64,
    ) -> Result<Arc<ScopedBootstrapBarrier>, ScopedObservationStartupError> {
        self.validate_host(host)?;
        let mut startup = self.lock_startup();
        if let StartupPhase::Live { .. } = startup.ordering.phase() {
            return host
                .engine_ready(drain)?
                .ok_or(ScopedObservationStartupError::InvalidPhase {
                    operation: "reuse bootstrap barrier",
                    phase: scoped_watcher_phase(&startup),
                });
        }
        if startup.ordering.phase() != StartupPhase::Reconciling {
            return Err(ScopedObservationStartupError::InvalidPhase {
                operation: "offer bootstrap complete",
                phase: scoped_watcher_phase(&startup),
            });
        }
        if startup.reconcile_pass_active {
            return Err(ScopedObservationStartupError::ReconcilePassActive);
        }
        let pending_hints = startup.ordering.pending_hint_count();
        if pending_hints > 0 {
            return Err(ScopedObservationStartupError::ReconcilePending { pending_hints });
        }
        let barrier = host.offer_consumer_bootstrap_complete(
            append_objects,
            admission,
            projection,
            drain,
            observed_at,
        )?;
        let transition = startup
            .ordering
            .finish_reconcile(barrier.barrier_sequence)
            .expect("startup lock keeps the verified empty hint set stable");
        debug_assert!(matches!(transition, StartupAction::Live { .. }));
        Ok(barrier)
    }
}

/// In-memory cursor/generation state for one exact append-delimited root. Its
/// first authorized reconciliation permanently binds it to that exact scope
/// relation, preventing later rebinding or duplicate coverage claims. It does
/// not own a store, query service, watcher, or public event queue.
pub struct ScopedKnownAppendObject {
    object_token: u64,
    access_identity: Option<ScopedAppendAccessIdentity>,
    source: ScopedSourceObjectIdentity,
    relation_id: Option<Arc<str>>,
    lifecycle: ScopedAppendObjectLifecycle,
    driver: AppendDelimitedFile,
    decoder: ScopedAppendDecoderConfig,
    checkpoint: Option<AppendCheckpoint>,
    decoder_state: Option<Vec<u8>>,
    bootstrap_active: bool,
    bootstrap_blocked: bool,
    presence_state: ScopedAppendPresenceState,
    next_admission_token: u64,
    pending: Option<PendingAppendState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedAppendAccessIdentity {
    object_token: AccessObjectToken,
    parent_token: Option<AccessObjectToken>,
    depth: u32,
    source_instance_id: u64,
    stream_id: u64,
    object_id: u64,
    media_type: SourceMediaType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedAppendObjectLifecycle {
    Active,
    Frozen {
        scope_epoch: u64,
        replacement_object_token: u64,
    },
    Replacement {
        scope_epoch: u64,
        replaces_object_token: u64,
    },
    Retired,
}

/// Presence knowledge for one exact object. `Unknown` is intentionally
/// distinct from `Missing`: an unstable read cannot establish absence and
/// therefore cannot make the next stable batch look like a creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedAppendPresenceState {
    Unknown,
    Missing,
    Present,
}

impl ScopedAppendPresenceState {
    fn is_known(self) -> bool {
        self != Self::Unknown
    }

    fn is_present(self) -> bool {
        self == Self::Present
    }

    fn observe_missing(
        self,
        previous_generation: Option<u64>,
    ) -> (Self, Option<ScopedAppendPresenceChange>, bool) {
        let became_missing = self == Self::Present;
        let change = became_missing.then(|| ScopedAppendPresenceChange::Deleted {
            generation: previous_generation
                .expect("a present scoped append object owns a checkpoint generation"),
        });
        (Self::Missing, change, became_missing)
    }

    fn observe_batch(self, generation: u64) -> (Self, Option<ScopedAppendPresenceChange>) {
        let change =
            (self == Self::Missing).then_some(ScopedAppendPresenceChange::Created { generation });
        (Self::Present, change)
    }
}

struct PendingAppendState {
    admission_token: u64,
    checkpoint: Option<AppendCheckpoint>,
    bootstrap_blocked: bool,
    presence_state: ScopedAppendPresenceState,
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
            access_identity: None,
            source,
            relation_id: None,
            lifecycle: ScopedAppendObjectLifecycle::Active,
            driver,
            decoder,
            checkpoint: None,
            decoder_state: None,
            bootstrap_active: true,
            bootstrap_blocked: false,
            presence_state: ScopedAppendPresenceState::Unknown,
            next_admission_token: 1,
            pending: None,
        })
    }

    pub fn reconcile(
        &mut self,
        pass: &ScopedObservationAccessPass,
        request: ScopedAppendReconcileRequest<'_>,
    ) -> Result<ScopedAppendObservation, ScopedObservationAccessError> {
        if matches!(
            self.lifecycle,
            ScopedAppendObjectLifecycle::Frozen { .. } | ScopedAppendObjectLifecycle::Retired
        ) {
            return Err(ScopedObservationAccessError::InvalidObjectLifecycle);
        }
        if self.pending.is_some() {
            return Err(ScopedObservationAccessError::ObservationPending);
        }
        pass.validate_source_identity(&self.source)?;
        pass.validate_known_relation(request.relation_id)?;
        match &self.relation_id {
            Some(bound) if bound.as_ref() != request.relation_id => {
                return Err(ScopedObservationAccessError::InvalidGrant(format!(
                    "scoped append object is already bound to relation {bound:?}"
                )));
            }
            Some(_) => {}
            None => self.relation_id = Some(Arc::from(request.relation_id)),
        }
        if self.access_identity.as_ref().is_some_and(|identity| {
            identity.parent_token != request.parent_token
                || identity.depth != request.depth
                || identity.source_instance_id != request.origin.source_instance_id
                || identity.stream_id != request.origin.stream_id
                || identity.object_id != request.origin.object_id
                || identity.media_type != request.origin.media_type
        }) {
            return Err(ScopedObservationAccessError::InvalidGrant(
                "known-object access or source identity changed after its first authorized access"
                    .to_string(),
            ));
        }
        let previous_generation = self.checkpoint.as_ref().map(|value| value.generation);
        let (read, access_object_token) = pass.read_known_append(
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
            self.access_identity
                .as_ref()
                .map(|identity| identity.object_token),
        )?;
        self.access_identity = Some(ScopedAppendAccessIdentity {
            object_token: access_object_token,
            parent_token: request.parent_token,
            depth: request.depth,
            source_instance_id: request.origin.source_instance_id,
            stream_id: request.origin.stream_id,
            object_id: request.origin.object_id,
            media_type: request.origin.media_type.clone(),
        });
        let (
            reset_before_items,
            presence_change,
            became_missing,
            next_checkpoint,
            next_presence_state,
            next_bootstrap_blocked,
        ) = match &read {
            AppendRead::Missing => {
                let (presence_state, presence_change, became_missing) =
                    self.presence_state.observe_missing(previous_generation);
                (
                    None,
                    presence_change,
                    became_missing,
                    self.checkpoint.clone(),
                    presence_state,
                    false,
                )
            }
            AppendRead::RetryTransient => (
                None,
                None,
                false,
                self.checkpoint.clone(),
                self.presence_state,
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
                let (presence_state, presence_change) =
                    self.presence_state.observe_batch(checkpoint.generation);
                (
                    reset,
                    presence_change,
                    false,
                    Some(checkpoint.clone()),
                    presence_state,
                    *more_available,
                )
            }
        };
        let phase = match self.lifecycle {
            ScopedAppendObjectLifecycle::Replacement { .. } => {
                ScopedAppendDeliveryPhase::Correction
            }
            ScopedAppendObjectLifecycle::Active if reset_before_items.is_some() => {
                ScopedAppendDeliveryPhase::Correction
            }
            ScopedAppendObjectLifecycle::Active if self.bootstrap_active => {
                ScopedAppendDeliveryPhase::Bootstrap
            }
            ScopedAppendObjectLifecycle::Active => ScopedAppendDeliveryPhase::Live,
            ScopedAppendObjectLifecycle::Frozen { .. } | ScopedAppendObjectLifecycle::Retired => {
                unreachable!("frozen and retired objects fail before source access")
            }
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
            presence_state: next_presence_state,
            staged_decoder_state: None,
        });
        Ok(ScopedAppendObservation {
            object_token: self.object_token,
            admission_token,
            access_pass_id: pass.pass_id,
            observed_at: request.origin.observed_at,
            phase,
            reset_before_items,
            presence_change,
            object_present: next_presence_state.is_present(),
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

    fn prepare_object_error_coverage(
        &self,
        error: &ScopedSourceObjectError,
    ) -> Result<ScopedOfferedDecodeCoverage, ()> {
        if !error.validate()
            || error.source != self.source
            || self.relation_id.as_deref() != Some(error.relation_id.as_ref())
        {
            return Err(());
        }
        let position = self
            .checkpoint
            .as_ref()
            .map(scoped_append_coverage_position)
            .transpose()?;
        if error.provenance.generation
            != self
                .checkpoint
                .as_ref()
                .map_or(1, |checkpoint| checkpoint.generation)
            || error.provenance.last_successful_position != position
        {
            return Err(());
        }
        let completeness = if error.retry.is_terminal() || position.is_none() {
            CoverageSetCompleteness::Unavailable
        } else {
            CoverageSetCompleteness::Partial
        };
        let status = if completeness == CoverageSetCompleteness::Partial {
            CoverageStatus::Partial
        } else {
            CoverageStatus::Unavailable {
                reason: error.failure_code.coverage_code().to_string(),
            }
        };
        let point = scoped_decode_coverage_point(
            &self.source,
            error.provenance.generation,
            position,
            status,
        )?;
        Ok(ScopedOfferedDecodeCoverage {
            source: self.source.clone(),
            point: Some(point),
            explicit_absence_or_deletion: None,
            explicit_errors: vec![CoverageError {
                stream_key: Some(self.source.stream_key),
                object_key: Some(self.source.object_key),
                code: error.failure_code.coverage_code().to_string(),
            }],
            completeness,
        })
    }

    fn object_error_provenance(
        &self,
    ) -> Result<ScopedSourceObjectErrorProvenance, ScopedObservationPassExecutionError> {
        let last_successful_position = self
            .checkpoint
            .as_ref()
            .map(scoped_append_coverage_position)
            .transpose()
            .map_err(|()| {
                ScopedObservationPassExecutionError::Admission(
                    ScopedAdmissionError::InvalidCoverage,
                )
            })?;
        Ok(ScopedSourceObjectErrorProvenance {
            generation: self
                .checkpoint
                .as_ref()
                .map_or(1, |checkpoint| checkpoint.generation),
            last_successful_position,
        })
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
        self.presence_state = pending.presence_state;
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
        if self.lifecycle != ScopedAppendObjectLifecycle::Active {
            return Err(ScopedObservationAccessError::InvalidObjectLifecycle);
        }
        if !self.bootstrap_active {
            return Err(ScopedObservationAccessError::BootstrapAlreadyComplete);
        }
        if self.pending.is_some() || !self.presence_state.is_known() || self.bootstrap_blocked {
            return Err(ScopedObservationAccessError::BootstrapNotDrained);
        }
        self.bootstrap_active = false;
        Ok(())
    }

    /// Create an empty source/cursor/decoder stage for a full-snapshot epoch.
    /// The active object is only a configuration and lineage template: none of
    /// its mutable source state is copied into replacement.
    pub fn fork_replacement(
        &mut self,
        scope_epoch: u64,
    ) -> Result<Self, ScopedObservationAccessError> {
        let lifecycle_allows_fork = match self.lifecycle {
            ScopedAppendObjectLifecycle::Active => scope_epoch > SCOPED_INITIAL_SCOPE_EPOCH,
            ScopedAppendObjectLifecycle::Frozen {
                scope_epoch: prior_epoch,
                ..
            } => scope_epoch > prior_epoch,
            ScopedAppendObjectLifecycle::Replacement { .. }
            | ScopedAppendObjectLifecycle::Retired => false,
        };
        if !lifecycle_allows_fork
            || self.bootstrap_active
            || self.pending.is_some()
            || self.relation_id.is_none()
        {
            return Err(ScopedObservationAccessError::InvalidObjectLifecycle);
        }
        let replacement_object_token = next_scoped_object_token()?;
        let replacement = Self {
            object_token: replacement_object_token,
            access_identity: self.access_identity.clone(),
            source: self.source.clone(),
            relation_id: self.relation_id.clone(),
            lifecycle: ScopedAppendObjectLifecycle::Replacement {
                scope_epoch,
                replaces_object_token: self.object_token,
            },
            driver: self.driver.clone(),
            decoder: self.decoder.clone(),
            checkpoint: None,
            decoder_state: None,
            bootstrap_active: false,
            bootstrap_blocked: false,
            presence_state: ScopedAppendPresenceState::Unknown,
            next_admission_token: 1,
            pending: None,
        };
        self.lifecycle = ScopedAppendObjectLifecycle::Frozen {
            scope_epoch,
            replacement_object_token,
        };
        Ok(replacement)
    }

    /// Validate the infallible object-state swap that a whole-scope completion
    /// barrier will eventually compose with coverage and reducer activation.
    pub fn validate_replacement_activation(
        &self,
        replacement: &Self,
        scope_epoch: u64,
    ) -> Result<(), ScopedObservationAccessError> {
        if self.lifecycle
            != (ScopedAppendObjectLifecycle::Frozen {
                scope_epoch,
                replacement_object_token: replacement.object_token,
            })
            || self.bootstrap_active
            || self.pending.is_some()
            || replacement.lifecycle
                != (ScopedAppendObjectLifecycle::Replacement {
                    scope_epoch,
                    replaces_object_token: self.object_token,
                })
            || replacement.pending.is_some()
            || !replacement.presence_state.is_known()
            || replacement.bootstrap_blocked
            || replacement.source != self.source
            || replacement.relation_id != self.relation_id
            || replacement.access_identity != self.access_identity
        {
            return Err(ScopedObservationAccessError::InvalidObjectLifecycle);
        }
        Ok(())
    }

    /// Swap a fully drained replacement into the active object slot. Callers
    /// exercise this only as component conformance; production activation is
    /// owned by the whole-scope post-barrier transfer.
    #[cfg(test)]
    pub fn activate_replacement(
        &mut self,
        replacement: &mut Self,
        scope_epoch: u64,
    ) -> Result<(), ScopedObservationAccessError> {
        self.validate_replacement_activation(replacement, scope_epoch)?;
        self.activate_replacement_prevalidated(replacement);
        Ok(())
    }

    fn activate_replacement_prevalidated(&mut self, replacement: &mut Self) {
        self.lifecycle = ScopedAppendObjectLifecycle::Retired;
        replacement.lifecycle = ScopedAppendObjectLifecycle::Active;
        std::mem::swap(self, replacement);
    }

    pub fn checkpoint(&self) -> Option<&AppendCheckpoint> {
        self.checkpoint.as_ref()
    }

    pub fn source_identity(&self) -> &ScopedSourceObjectIdentity {
        &self.source
    }

    pub fn relation_id(&self) -> Option<&str> {
        self.relation_id.as_deref()
    }

    fn coverage_membership_identity(&self) -> ScopedCoverageMembershipIdentity {
        ScopedCoverageMembershipIdentity {
            relation_id: Arc::clone(
                self.relation_id
                    .as_ref()
                    .expect("admitted scoped append object is bound to an exact relation"),
            ),
            stream_key: Arc::from(self.decoder.semantic_context.stream_key()),
            object_key: Arc::from(self.decoder.semantic_context.object_key()),
            coverage_domains: self.decoder.coverage_domains.clone(),
        }
    }

    pub fn decoder_state(&self) -> Option<&[u8]> {
        self.decoder_state.as_deref()
    }

    pub fn is_present(&self) -> bool {
        self.presence_state.is_present()
    }

    pub fn bootstrap_active(&self) -> bool {
        self.bootstrap_active
    }

    pub fn replacement_scope_epoch(&self) -> Option<u64> {
        match self.lifecycle {
            ScopedAppendObjectLifecycle::Replacement { scope_epoch, .. } => Some(scope_epoch),
            ScopedAppendObjectLifecycle::Active
            | ScopedAppendObjectLifecycle::Frozen { .. }
            | ScopedAppendObjectLifecycle::Retired => None,
        }
    }

    pub fn frozen_scope_epoch(&self) -> Option<u64> {
        match self.lifecycle {
            ScopedAppendObjectLifecycle::Frozen { scope_epoch, .. } => Some(scope_epoch),
            ScopedAppendObjectLifecycle::Active
            | ScopedAppendObjectLifecycle::Replacement { .. }
            | ScopedAppendObjectLifecycle::Retired => None,
        }
    }

    pub fn is_retired(&self) -> bool {
        self.lifecycle == ScopedAppendObjectLifecycle::Retired
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

fn validate_scoped_relation_coverage(
    known_objects: &BTreeMap<String, ScopedKnownObjectGrant>,
    admission: &ScopedObservationAdmissionLane,
) -> Result<(), ScopedCoverageAssemblyError> {
    if known_objects.len() != admission.known_coverage_objects.len()
        || known_objects.keys().any(|relation_id| {
            !admission
                .known_coverage_objects
                .values()
                .any(|membership| membership.relation_id.as_ref() == relation_id)
        })
        || admission
            .known_coverage_objects
            .values()
            .any(|membership| !known_objects.contains_key(membership.relation_id.as_ref()))
    {
        return Err(ScopedCoverageAssemblyError::DeclaredObjectCoverageMismatch);
    }
    Ok(())
}

fn scoped_object_errors_match_epoch(active: &ScopedObservationEpochState) -> bool {
    active.object_errors.iter().all(|(relation_id, state)| {
        active
            .append_objects
            .get(relation_id)
            .is_some_and(|object| {
                state.error.validate()
                    && state.error.relation_id.as_ref() == relation_id
                    && state.error.scope_epoch == active.scope_epoch
                    && state.error.source == object.source
            })
    })
}

fn validate_bootstrap_source_state(
    known_objects: &BTreeMap<String, ScopedKnownObjectGrant>,
    root_relation_id: &str,
    root: &ScopedObservationRootIdentity,
    admission: &ScopedObservationAdmissionLane,
    objects: &[ScopedKnownAppendObject],
) -> Result<bool, ScopedBootstrapBarrierError> {
    validate_scoped_relation_coverage(known_objects, admission)
        .map_err(ScopedBootstrapBarrierError::Coverage)?;
    if objects.len() != known_objects.len() {
        return Err(ScopedBootstrapBarrierError::InvalidSnapshot);
    }

    let mut bound = BTreeMap::new();
    for object in objects {
        let relation_id = object
            .relation_id
            .as_deref()
            .ok_or(ScopedBootstrapBarrierError::InvalidSnapshot)?;
        let membership = admission
            .known_coverage_objects
            .get(&object.source)
            .ok_or(ScopedBootstrapBarrierError::InvalidSnapshot)?;
        if object.lifecycle != ScopedAppendObjectLifecycle::Active
            || object.bootstrap_active
            || object.pending.is_some()
            || !object.presence_state.is_known()
            || !source_belongs_to_root(&object.source, root)
            || !known_objects.contains_key(relation_id)
            || membership.relation_id.as_ref() != relation_id
            || bound.insert(relation_id, object).is_some()
        {
            return Err(ScopedBootstrapBarrierError::InvalidSnapshot);
        }
    }
    if bound.keys().any(|key| !known_objects.contains_key(*key))
        || known_objects
            .keys()
            .any(|key| !bound.contains_key(key.as_str()))
    {
        return Err(ScopedBootstrapBarrierError::InvalidSnapshot);
    }
    bound
        .get(root_relation_id)
        .map(|object| object.is_present())
        .ok_or(ScopedBootstrapBarrierError::InvalidSnapshot)
}

fn bind_active_scoped_append_objects(
    known_objects: &BTreeMap<String, ScopedKnownObjectGrant>,
    root: &ScopedObservationRootIdentity,
    admission: &ScopedObservationAdmissionLane,
    objects: Vec<ScopedKnownAppendObject>,
) -> Result<BTreeMap<String, ScopedKnownAppendObject>, ScopedReplacementStageError> {
    validate_scoped_relation_coverage(known_objects, admission)
        .map_err(ScopedReplacementStageError::Coverage)?;
    if objects.len() != known_objects.len() {
        return Err(ScopedReplacementStageError::InvalidSourceState);
    }
    let mut bound = BTreeMap::new();
    for object in objects {
        let relation_id = object
            .relation_id
            .as_deref()
            .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
        let membership = admission
            .known_coverage_objects
            .get(&object.source)
            .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
        if object.lifecycle != ScopedAppendObjectLifecycle::Active
            || object.bootstrap_active
            || object.pending.is_some()
            || !source_belongs_to_root(&object.source, root)
            || !known_objects.contains_key(relation_id)
            || membership.relation_id.as_ref() != relation_id
        {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
        if bound.insert(relation_id.to_string(), object).is_some() {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
    }
    if bound.keys().any(|key| !known_objects.contains_key(key))
        || known_objects.keys().any(|key| !bound.contains_key(key))
    {
        return Err(ScopedReplacementStageError::InvalidSourceState);
    }
    Ok(bound)
}

fn validate_scope_replacement_source_state(
    known_objects: &BTreeMap<String, ScopedKnownObjectGrant>,
    root: &ScopedObservationRootIdentity,
    active: &ScopedObservationEpochState,
    stage: &ScopedObservationScopeReplacementStage,
    delivery: &ScopedObservationDeliveryLane,
) -> Result<(), ScopedReplacementStageError> {
    stage.semantic.validate_delivery(delivery)?;
    let started = delivery
        .resync_started
        .as_ref()
        .ok_or(ScopedReplacementStageError::NotResyncing)?;
    let baseline_scope_epoch = valid_replacement_baseline_scope_epoch(delivery)
        .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
    validate_scoped_relation_coverage(known_objects, &stage.admission)
        .map_err(ScopedReplacementStageError::Coverage)?;
    if active.root != *root
        || stage.semantic.root != *root
        || active.scope_epoch != baseline_scope_epoch
        || stage.semantic.scope_epoch != started.new_scope_epoch
        || !stage.admission.is_empty()
        || active.append_objects.len() != known_objects.len()
        || stage.append_objects.len() != known_objects.len()
        || active.append_objects.keys().ne(stage.append_objects.keys())
    {
        return Err(ScopedReplacementStageError::InvalidSourceState);
    }
    for (relation_id, active_object) in &active.append_objects {
        let replacement = stage
            .append_objects
            .get(relation_id)
            .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
        active_object
            .validate_replacement_activation(replacement, started.new_scope_epoch)
            .map_err(|_| ScopedReplacementStageError::InvalidSourceState)?;
        let membership = stage
            .admission
            .known_coverage_objects
            .get(&replacement.source)
            .ok_or(ScopedReplacementStageError::InvalidSourceState)?;
        if membership.relation_id.as_ref() != relation_id
            || !source_belongs_to_root(&replacement.source, root)
        {
            return Err(ScopedReplacementStageError::InvalidSourceState);
        }
    }
    Ok(())
}

fn valid_replacement_baseline_scope_epoch(delivery: &ScopedObservationDeliveryLane) -> Option<u64> {
    delivery
        .resync_barrier
        .as_ref()
        .map(|barrier| barrier.scope_epoch)
        .or_else(|| {
            delivery
                .bootstrap_barrier
                .as_ref()
                .map(|barrier| barrier.scope_epoch)
        })
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
    if validated.values().filter(|grant| grant.scope_root).count() != 1 {
        return Err(ScopedObservationAccessError::InvalidGrant(
            "exactly one known-object grant must be designated as the scope root".to_string(),
        ));
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
fn scoped_source_owner_error_is_transient(error: &ScopedObservationPassExecutionError) -> bool {
    matches!(
        error,
        ScopedObservationPassExecutionError::DecodeRetryTransient
            | ScopedObservationPassExecutionError::Access(
                ScopedObservationAccessError::Decode(ScopedDecodeFailureClass::Transient)
                    | ScopedObservationAccessError::Source(
                        ScopedSourceFailureClass::Unstable
                            | ScopedSourceFailureClass::Database
                            | ScopedSourceFailureClass::Io
                    )
            )
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

    #[test]
    fn transient_initial_read_cannot_fabricate_source_creation() {
        let unknown = ScopedAppendPresenceState::Unknown;

        // RetryTransient deliberately preserves Unknown. A subsequent stable
        // batch is cold bootstrap evidence, not proof that the source appeared.
        let after_transient = unknown;
        assert!(!after_transient.is_known());
        let (present, change) = after_transient.observe_batch(1);
        assert_eq!(present, ScopedAppendPresenceState::Present);
        assert_eq!(change, None);

        // A creation control is justified only by a prior stable Missing read.
        let (missing, deletion, became_missing) = unknown.observe_missing(None);
        assert_eq!(missing, ScopedAppendPresenceState::Missing);
        assert_eq!(deletion, None);
        assert!(!became_missing);
        let (present, change) = missing.observe_batch(1);
        assert_eq!(present, ScopedAppendPresenceState::Present);
        assert_eq!(
            change,
            Some(ScopedAppendPresenceChange::Created { generation: 1 })
        );
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

    fn consumer_drain(
        root: ScopedObservationRootIdentity,
        max_semantic_events: usize,
        max_source_control_items: usize,
    ) -> ScopedObservationConsumerDrain {
        let lifecycle = Arc::new(ScopedObservationAttachmentLifecycle::default());
        let mut drain = ScopedObservationConsumerDrain::new(
            ScopedObservationEnvelopeMapper::new(root),
            Arc::new(ScopedObservationAttachmentAuthority),
            Arc::clone(&lifecycle),
            ScopedObservationDeliveryLimits {
                max_semantic_events,
                max_retained_native_bytes: 0,
                max_source_control_items,
            },
        )
        .unwrap();
        lifecycle
            .open_consumer_drain(&drain.delivery.event_completion)
            .unwrap();
        drain.lifecycle_registered = true;
        drain
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
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), true, 99)
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
    fn scoped_consumer_drain_separates_engine_and_applied_bootstrap_readiness() {
        let root = root_identity();
        let source = source_identity();
        let creation = ScopedAppendPresenceChange::Created { generation: 1 };
        let bootstrap_presence = ScopedProjectedObservation::SourcePresence {
            object_token: OBJECT_TOKEN,
            source: source.clone(),
            lane_ordinal: 1,
            observed_at: 40,
            phase: ScopedAppendDeliveryPhase::Bootstrap,
            event_id: source_presence_event_id(&source, creation),
            change: creation,
        };
        let mut drain = consumer_drain(root.clone(), 1, 2);
        drain
            .delivery_lane_mut()
            .offer(vec![bootstrap_presence.clone()])
            .unwrap();
        let watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            offered_through_sequence: 1,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: drain.delivery_lane().state(),
        };
        let engine_barrier = drain
            .delivery_lane_mut()
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), true, 50)
            .unwrap();
        assert_eq!(engine_barrier.barrier_sequence, 2);
        assert!(Arc::ptr_eq(
            &drain.engine_bootstrap_barrier().unwrap(),
            &engine_barrier
        ));
        assert_eq!(
            drain.state(),
            ScopedObservationAppliedState {
                delivered_through_sequence: 0,
                applied_through_sequence: 0,
                applied_scope_epoch: None,
                pending_sequence: None,
                bootstrap_barrier_sequence: None,
                resync_barrier_sequence: None,
            }
        );

        let first = drain.next().unwrap().unwrap();
        assert_eq!(first.envelope.observer_sequence, 1);
        assert!(matches!(
            first.envelope.event,
            ScopedObservationEvent::SourcePresence { change } if change == creation
        ));
        let first_receipt = first.application_receipt().clone();
        assert_eq!(first_receipt.observer_sequence(), 1);
        assert_eq!(first_receipt.scope_epoch(), SCOPED_INITIAL_SCOPE_EPOCH);
        assert_eq!(first_receipt.event_id(), bootstrap_presence.event_id());
        assert!(matches!(
            drain.next(),
            Err(ScopedObservationDrainError::ApplicationPending)
        ));
        assert!(drain.consumer_bootstrap_barrier().is_none());

        // Even an identical replay coordinate from another drain cannot
        // acknowledge this attachment's pending application.
        let mut foreign_drain = consumer_drain(root.clone(), 1, 1);
        foreign_drain
            .delivery_lane_mut()
            .offer(vec![bootstrap_presence.clone()])
            .unwrap();
        let foreign = foreign_drain.next().unwrap().unwrap();
        assert_eq!(foreign.envelope.observer_sequence, 1);
        assert_eq!(foreign.envelope.event_id, first_receipt.event_id());
        assert_eq!(
            drain.acknowledge_applied(foreign.application_receipt()),
            Err(ScopedObservationApplicationError::ForeignReceipt)
        );
        assert_eq!(drain.state().pending_sequence, Some(1));

        let mut mismatched = first_receipt.clone();
        mismatched.observer_sequence += 1;
        assert_eq!(
            drain.acknowledge_applied(&mismatched),
            Err(ScopedObservationApplicationError::ReceiptMismatch)
        );
        let applied_first = drain.acknowledge_applied(&first_receipt).unwrap();
        assert_eq!(applied_first.applied_through_sequence, 1);
        assert_eq!(applied_first.pending_sequence, None);
        assert_eq!(
            drain.acknowledge_applied(&first_receipt).unwrap(),
            applied_first
        );

        let complete = drain.next().unwrap().unwrap();
        assert_eq!(complete.envelope.observer_sequence, 2);
        assert!(matches!(
            &complete.envelope.event,
            ScopedObservationEvent::ObserverBootstrapComplete { barrier }
                if Arc::ptr_eq(barrier, &engine_barrier)
        ));
        let complete_receipt = complete.application_receipt().clone();
        assert!(drain.consumer_bootstrap_barrier().is_none());
        // A retry of the previous acknowledgement is harmless and cannot skip
        // the currently pending completion barrier.
        assert_eq!(
            drain.acknowledge_applied(&first_receipt).unwrap(),
            drain.state()
        );
        assert_eq!(drain.state().pending_sequence, Some(2));
        let ready = drain.acknowledge_applied(&complete_receipt).unwrap();
        assert_eq!(ready.applied_through_sequence, 2);
        assert_eq!(ready.bootstrap_barrier_sequence, Some(2));
        assert!(Arc::ptr_eq(
            &drain.consumer_bootstrap_barrier().unwrap(),
            &engine_barrier
        ));
        assert!(drain.next().unwrap().is_none());
    }

    #[test]
    fn scoped_consumer_drain_mapping_failure_does_not_dequeue() {
        let root = root_identity();
        let mut foreign_source = source_identity();
        foreign_source.source_instance_key =
            CanonicalSourceInstanceKey::derive(1, b"foreign-source-instance").unwrap();
        let change = ScopedAppendPresenceChange::Created { generation: 1 };
        let mut drain = consumer_drain(root, 1, 1);
        drain
            .delivery_lane_mut()
            .offer(vec![ScopedProjectedObservation::SourcePresence {
                object_token: OBJECT_TOKEN,
                source: foreign_source.clone(),
                lane_ordinal: 1,
                observed_at: 10,
                phase: ScopedAppendDeliveryPhase::Bootstrap,
                event_id: source_presence_event_id(&foreign_source, change),
                change,
            }])
            .unwrap();
        let queued = drain.delivery_lane().state();

        assert!(matches!(
            drain.next(),
            Err(ScopedObservationDrainError::Envelope(
                ScopedEnvelopeError::RootSourceMismatch
            ))
        ));
        assert_eq!(drain.delivery_lane().state(), queued);
        assert_eq!(drain.delivery_lane().queued_source_control_items(), 1);
        assert_eq!(drain.state().delivered_through_sequence, 0);
        assert_eq!(drain.state().pending_sequence, None);
        assert!(matches!(
            drain.next(),
            Err(ScopedObservationDrainError::Envelope(
                ScopedEnvelopeError::RootSourceMismatch
            ))
        ));
        assert_eq!(drain.delivery_lane().state(), queued);
    }

    #[test]
    fn scoped_consumer_close_cancels_pending_receipt_and_queued_delivery() {
        let root = root_identity();
        let source = source_identity();
        let created = ScopedAppendPresenceChange::Created { generation: 1 };
        let deleted = ScopedAppendPresenceChange::Deleted { generation: 1 };
        let mut drain = consumer_drain(root, 1, 2);
        drain
            .delivery_lane_mut()
            .offer(vec![
                ScopedProjectedObservation::SourcePresence {
                    object_token: OBJECT_TOKEN,
                    source: source.clone(),
                    lane_ordinal: 1,
                    observed_at: 10,
                    phase: ScopedAppendDeliveryPhase::Live,
                    event_id: source_presence_event_id(&source, created),
                    change: created,
                },
                ScopedProjectedObservation::SourcePresence {
                    object_token: OBJECT_TOKEN,
                    source: source.clone(),
                    lane_ordinal: 2,
                    observed_at: 20,
                    phase: ScopedAppendDeliveryPhase::Live,
                    event_id: source_presence_event_id(&source, deleted),
                    change: deleted,
                },
            ])
            .unwrap();
        let yielded = drain.next().unwrap().unwrap();
        let receipt = yielded.application_receipt().clone();
        assert_eq!(drain.state().pending_sequence, Some(1));
        assert_eq!(drain.delivery_lane().queued_source_control_items(), 1);

        let barrier = drain.lifecycle.begin_close();
        let closing = barrier.state();
        assert!(closing.close_requested);
        assert!(closing.consumer_drain_pending);
        assert!(!closing.complete);
        assert!(matches!(
            drain.next(),
            Err(ScopedObservationDrainError::Closed)
        ));
        let closed_state = drain.state();
        assert!(drain.is_closed());
        assert_eq!(closed_state.pending_sequence, None);
        assert!(drain.delivery_lane().is_empty());
        assert_eq!(
            drain.acknowledge_applied(&receipt),
            Err(ScopedObservationApplicationError::Closed)
        );
        assert!(matches!(
            drain.next(),
            Err(ScopedObservationDrainError::Closed)
        ));
        assert!(barrier.wait().complete);
    }

    #[tokio::test]
    async fn scoped_event_async_wait_wakes_and_retains_pre_poll_state() {
        let completion = Arc::new(ScopedObservationEventCompletion::default());
        let waiter = ScopedObservationEventWaiter {
            completion: Arc::clone(&completion),
        };
        let waiting = waiter.clone();
        let wait_task = tokio::spawn(async move { waiting.wait_after_async(0).await });
        tokio::task::yield_now().await;

        completion.publish(1);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), wait_task)
                .await
                .unwrap()
                .unwrap(),
            ScopedObservationEventWakeState {
                offered_through_sequence: 1,
                closed: false,
            }
        );

        // Constructing an async wait does not subscribe until first poll. A
        // close in that interval must still resolve from retained state.
        let closed = waiter.wait_after_async(1);
        completion.close();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), closed)
                .await
                .unwrap(),
            ScopedObservationEventWakeState {
                offered_through_sequence: 1,
                closed: true,
            }
        );
    }

    #[test]
    fn scoped_consumer_drain_applies_explicit_sequence_gap_after_invalidation() {
        let root = root_identity();
        let source = source_identity();
        let mut drain = consumer_drain(root.clone(), 1, 3);
        let event_waiter = drain.event_waiter();
        assert_eq!(event_waiter.snapshot().offered_through_sequence, 0);
        let watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            offered_through_sequence: 0,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: drain.delivery_lane().state(),
        };
        let bootstrap = drain
            .delivery_lane_mut()
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), false, 10)
            .unwrap();
        assert_eq!(event_waiter.snapshot().offered_through_sequence, 1);
        let bootstrap_delivery = drain.next().unwrap().unwrap();
        drain
            .acknowledge_applied(bootstrap_delivery.application_receipt())
            .unwrap();
        assert_eq!(drain.state().applied_through_sequence, 1);
        assert!(Arc::ptr_eq(
            &drain.consumer_bootstrap_barrier().unwrap(),
            &bootstrap
        ));

        let created = ScopedAppendPresenceChange::Created { generation: 1 };
        let deleted = ScopedAppendPresenceChange::Deleted { generation: 1 };
        drain
            .delivery_lane_mut()
            .offer(vec![
                ScopedProjectedObservation::SourcePresence {
                    object_token: OBJECT_TOKEN,
                    source: source.clone(),
                    lane_ordinal: 2,
                    observed_at: 20,
                    phase: ScopedAppendDeliveryPhase::Live,
                    event_id: source_presence_event_id(&source, created),
                    change: created,
                },
                ScopedProjectedObservation::SourcePresence {
                    object_token: OBJECT_TOKEN,
                    source: source.clone(),
                    lane_ordinal: 3,
                    observed_at: 21,
                    phase: ScopedAppendDeliveryPhase::Live,
                    event_id: source_presence_event_id(&source, deleted),
                    change: deleted,
                },
            ])
            .unwrap();
        assert_eq!(event_waiter.snapshot().offered_through_sequence, 3);
        let required = drain
            .delivery_lane_mut()
            .require_resync(&root, ScopedResyncReason::WatcherOverflow, 30)
            .unwrap();
        assert_eq!(event_waiter.snapshot().offered_through_sequence, 4);
        assert_eq!(required.control_sequence, 4);
        assert_eq!(required.last_contiguous_sequence, 1);
        assert_eq!(required.discarded_source_controls, 2);

        let invalidation = drain.next().unwrap().unwrap();
        assert_eq!(invalidation.envelope.observer_sequence, 4);
        assert!(matches!(
            &invalidation.envelope.event,
            ScopedObservationEvent::ObserverResyncRequired { control }
                if Arc::ptr_eq(control, &required)
        ));
        let state = drain
            .acknowledge_applied(invalidation.application_receipt())
            .unwrap();
        assert_eq!(state.delivered_through_sequence, 4);
        assert_eq!(state.applied_through_sequence, 4);
        assert_eq!(state.applied_scope_epoch, Some(1));
        assert_eq!(state.pending_sequence, None);

        let started = drain.delivery_lane_mut().begin_resync(&root, 40).unwrap();
        assert_eq!(event_waiter.snapshot().offered_through_sequence, 5);
        assert_eq!(started.control_sequence, 5);
        let started_delivery = drain.next().unwrap().unwrap();
        assert!(matches!(
            &started_delivery.envelope.event,
            ScopedObservationEvent::ObserverResyncStarted { control }
                if Arc::ptr_eq(control, &started)
        ));
        drain
            .acknowledge_applied(started_delivery.application_receipt())
            .unwrap();
        assert_eq!(drain.state().applied_scope_epoch, Some(2));

        let completion_watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: 2,
            offered_through_sequence: 5,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: drain.delivery_lane().state(),
        };
        let replacement_digest = replacement_snapshot_digest(&root, false, &[], &[], &[]).unwrap();
        let resync_barrier = drain
            .delivery_lane_mut()
            .offer_resync_barrier(
                &root,
                completion_watermark,
                Vec::new(),
                replacement_digest,
                false,
                50,
            )
            .unwrap();
        assert_eq!(event_waiter.snapshot().offered_through_sequence, 6);
        assert_eq!(resync_barrier.barrier_sequence, 6);
        assert!(Arc::ptr_eq(
            &drain.engine_resync_barrier().unwrap(),
            &resync_barrier
        ));
        let completed = drain.next().unwrap().unwrap();
        assert!(matches!(
            &completed.envelope.event,
            ScopedObservationEvent::ObserverResyncComplete { barrier }
                if Arc::ptr_eq(barrier, &resync_barrier)
        ));
        assert!(drain.consumer_resync_barrier().is_none());
        let state = drain
            .acknowledge_applied(completed.application_receipt())
            .unwrap();
        assert_eq!(state.applied_through_sequence, 6);
        assert_eq!(state.applied_scope_epoch, Some(2));
        assert_eq!(state.resync_barrier_sequence, Some(6));
        assert!(Arc::ptr_eq(
            &drain.consumer_resync_barrier().unwrap(),
            &resync_barrier
        ));
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
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), true, 50)
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
        assert_eq!(
            control.baseline_snapshot_digest,
            barrier.replacement_snapshot_digest
        );
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
        assert_eq!(
            started.baseline_snapshot_digest,
            barrier.replacement_snapshot_digest
        );
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
        assert_eq!(
            delivery.require_resync(&root, ScopedResyncReason::TransportContinuityLoss, 115),
            Err(ScopedContinuityError::ResyncStartNotDelivered)
        );
        assert_eq!(delivery.state().offered_through_sequence, 7);
        assert_eq!(delivery.queued_source_control_items(), 2);

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
    fn scoped_observer_failure_supersedes_backlog_and_is_terminal() {
        let root = root_identity();
        let mapper = ScopedObservationEnvelopeMapper::new(root.clone());
        let mut delivery = delivery_lane(2, 2);
        let watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: SCOPED_INITIAL_SCOPE_EPOCH,
            offered_through_sequence: 0,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: delivery.state(),
        };
        delivery
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), false, 10)
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);

        let live_record = record(1, 0, 10);
        let live_frame = decoded_frame(
            1,
            ScopedAppendDeliveryPhase::Live,
            &live_record,
            usage_batch(&live_record, "terminal-response", 10, None),
        );
        let mut projection = sink(2);
        delivery
            .offer(projection.project(&live_frame).unwrap())
            .unwrap();
        let source = source_identity();
        let created = ScopedAppendPresenceChange::Created { generation: 1 };
        delivery
            .offer(vec![ScopedProjectedObservation::SourcePresence {
                object_token: OBJECT_TOKEN,
                source: source.clone(),
                lane_ordinal: 2,
                observed_at: 20,
                phase: ScopedAppendDeliveryPhase::Live,
                event_id: source_presence_event_id(&source, created),
                change: created,
            }])
            .unwrap();

        let failure = delivery
            .fail_observer(
                &root,
                ScopedObserverFailureReason::NativeWatcherRecoveryExhausted,
                30,
            )
            .unwrap();
        assert_eq!(failure.failed_scope_epoch, 1);
        assert_eq!(failure.control_sequence, 4);
        assert_eq!(failure.last_contiguous_sequence, 1);
        assert_eq!(failure.phase, ScopedAppendDeliveryPhase::Live);
        assert_eq!(failure.discarded_semantic_events, 1);
        assert_eq!(failure.discarded_source_controls, 1);
        assert_eq!(failure.discarded_retained_native_bytes, 0);
        assert_eq!(delivery.queued_semantic_events(), 0);
        assert_eq!(delivery.queued_source_control_items(), 1);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::Failed
        );
        assert!(Arc::ptr_eq(&failure, &delivery.observer_failure().unwrap()));

        let repeated = delivery
            .fail_observer(
                &root,
                ScopedObserverFailureReason::InternalControlFailure,
                999,
            )
            .unwrap();
        assert!(Arc::ptr_eq(&failure, &repeated));
        assert_eq!(delivery.queued_source_control_items(), 1);
        assert_eq!(
            delivery.require_resync(&root, ScopedResyncReason::WatcherOverflow, 40),
            Err(ScopedContinuityError::ObserverFailed)
        );
        assert_eq!(
            delivery.begin_resync(&root, 50),
            Err(ScopedContinuityError::ObserverFailed)
        );

        let late = ScopedProjectedObservation::SourcePresence {
            object_token: OBJECT_TOKEN,
            source: source.clone(),
            lane_ordinal: 3,
            observed_at: 60,
            phase: ScopedAppendDeliveryPhase::Live,
            event_id: source_presence_event_id(
                &source,
                ScopedAppendPresenceChange::Deleted { generation: 1 },
            ),
            change: ScopedAppendPresenceChange::Deleted { generation: 1 },
        };
        let rejected = delivery.offer(vec![late]).unwrap_err();
        assert_eq!(rejected.error, ScopedDeliveryError::ObserverFailed);
        assert_eq!(rejected.projected.len(), 1);

        let delivered = delivery.pop_next().unwrap();
        assert_eq!(delivered.observer_sequence, 4);
        assert_eq!(
            delivered.event_id,
            observer_failed_event_id(
                &root,
                1,
                ScopedObserverFailureReason::NativeWatcherRecoveryExhausted,
            )
        );
        let envelope = mapper.map(delivered).unwrap();
        assert_eq!(envelope.observed_at, 30);
        assert_eq!(envelope.semantic_revision_ref, None);
        assert_eq!(
            envelope.actor_attribution,
            ScopedActorAttribution::ScopeFallback {
                reason: ScopedActorFallbackReason::ObserverLifecycleControl,
            }
        );
        assert!(matches!(
            envelope.event,
            ScopedObservationEvent::ObserverFailed {
                failure: delivered_failure,
            } if Arc::ptr_eq(&failure, &delivered_failure)
        ));
        assert!(delivery.is_empty());

        let mut replay = delivery_lane(1, 1);
        let replay_watermark = ScopedObservationWatermarkCore {
            root: root.clone(),
            scope_epoch: 1,
            offered_through_sequence: 0,
            source_coverage: Vec::new(),
            explicit_object_errors: Vec::new(),
            queue_state: replay.state(),
        };
        replay
            .offer_bootstrap_barrier(&root, replay_watermark, Vec::new(), false, 777)
            .unwrap();
        replay.pop_next().unwrap();
        let replay_failure = replay
            .fail_observer(
                &root,
                ScopedObserverFailureReason::NativeWatcherRecoveryExhausted,
                888,
            )
            .unwrap();
        assert_ne!(failure.control_sequence, replay_failure.control_sequence);
        assert_eq!(
            observer_failed_event_id(&root, failure.failed_scope_epoch, failure.reason,),
            observer_failed_event_id(
                &root,
                replay_failure.failed_scope_epoch,
                replay_failure.reason,
            )
        );
    }

    #[test]
    fn scoped_native_watcher_recovery_policy_is_bounded_and_caps_backoff() {
        assert_eq!(
            ScopedObservationNativeWatcherRecoveryPolicy::new(
                Duration::ZERO,
                Duration::from_millis(1),
                Duration::from_millis(1),
                1,
            ),
            Err(ScopedObservationNativeWatcherRecoveryPolicyError::AuditInterval)
        );
        assert_eq!(
            ScopedObservationNativeWatcherRecoveryPolicy::new(
                Duration::from_secs(1),
                Duration::from_millis(2),
                Duration::from_millis(1),
                1,
            ),
            Err(ScopedObservationNativeWatcherRecoveryPolicyError::RetryInterval)
        );
        assert_eq!(
            ScopedObservationNativeWatcherRecoveryPolicy::new(
                Duration::from_secs(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
                0,
            ),
            Err(ScopedObservationNativeWatcherRecoveryPolicyError::AttemptLimit)
        );

        let policy = ScopedObservationNativeWatcherRecoveryPolicy::new(
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(25),
            4,
        )
        .unwrap();
        assert_eq!(policy.retry_delay(0), Duration::from_millis(10));
        assert_eq!(policy.retry_delay(1), Duration::from_millis(20));
        assert_eq!(policy.retry_delay(2), Duration::from_millis(25));
        assert_eq!(policy.retry_delay(31), Duration::from_millis(25));
        assert_eq!(policy.max_reinstall_attempts(), 4);
    }

    #[test]
    fn scoped_source_owner_retry_policy_and_failure_classes_are_bounded() {
        assert_eq!(
            ScopedObservationSourceOwnerRetryPolicy::new(
                Duration::ZERO,
                Duration::from_millis(1),
                1,
            ),
            Err(ScopedObservationSourceOwnerRetryPolicyError::RetryInterval)
        );
        assert_eq!(
            ScopedObservationSourceOwnerRetryPolicy::new(
                Duration::from_millis(2),
                Duration::from_millis(1),
                1,
            ),
            Err(ScopedObservationSourceOwnerRetryPolicyError::RetryInterval)
        );
        assert_eq!(
            ScopedObservationSourceOwnerRetryPolicy::new(
                Duration::from_millis(1),
                Duration::from_millis(1),
                0,
            ),
            Err(ScopedObservationSourceOwnerRetryPolicyError::AttemptLimit)
        );
        let policy = ScopedObservationSourceOwnerRetryPolicy::new(
            Duration::from_millis(10),
            Duration::from_millis(25),
            4,
        )
        .unwrap();
        assert_eq!(policy.retry_delay(0), Duration::from_millis(10));
        assert_eq!(policy.retry_delay(1), Duration::from_millis(20));
        assert_eq!(policy.retry_delay(2), Duration::from_millis(25));

        let valid_error = ScopedSourceObjectError {
            error_contract_version: SCOPED_SOURCE_OBJECT_ERROR_CONTRACT_VERSION,
            relation_id: Arc::from("root-object"),
            source: source_identity(),
            scope_epoch: 1,
            failure_code: ScopedSourceObjectFailureCode::DecodeRetryTransient,
            provenance: ScopedSourceObjectErrorProvenance {
                generation: 1,
                last_successful_position: None,
            },
            retry: ScopedSourceObjectRetryState::RetryScheduled {
                failed_attempts: 1,
                max_attempts: 4,
                retry_after_ms: 10,
            },
        };
        assert!(valid_error.validate());
        assert!(policy.accepts_retained_retry(valid_error.retry));

        let mut invalid_relation = valid_error.clone();
        invalid_relation.relation_id = Arc::from("Root Object");
        assert!(!invalid_relation.validate());

        let mut mismatched_class = valid_error.clone();
        mismatched_class.failure_code = ScopedSourceObjectFailureCode::DecodeStreamFatal;
        assert!(!mismatched_class.validate());

        let mut wrong_position_kind = valid_error.clone();
        wrong_position_kind.provenance.last_successful_position = Some(
            CoveragePosition::derive(
                CoveragePositionKind::DocumentRevision,
                b"revision-1",
                Some(1),
            )
            .unwrap(),
        );
        assert!(!wrong_position_kind.validate());

        let mut missing_position_order = valid_error.clone();
        missing_position_order.provenance.last_successful_position = Some(
            CoveragePosition::derive(CoveragePositionKind::AppendCursor, b"offset-1", None)
                .unwrap(),
        );
        assert!(!missing_position_order.validate());

        let incompatible_attempt_ceiling = ScopedObservationSourceOwnerRetryPolicy::new(
            Duration::from_millis(10),
            Duration::from_millis(25),
            3,
        )
        .unwrap();
        assert!(!incompatible_attempt_ceiling.accepts_retained_retry(valid_error.retry));
        let incompatible_delay_ceiling = ScopedObservationSourceOwnerRetryPolicy::new(
            Duration::from_millis(5),
            Duration::from_millis(5),
            4,
        )
        .unwrap();
        assert!(!incompatible_delay_ceiling.accepts_retained_retry(valid_error.retry));

        for delivery_error in [
            ScopedDeliveryError::SemanticQueueFull,
            ScopedDeliveryError::RetainedNativeQueueFull,
            ScopedDeliveryError::SourceControlQueueFull,
        ] {
            let error = ScopedObservationPassExecutionError::Offer(
                ScopedObservationConsumerOfferError::Offer(
                    ScopedProjectionDeliveryError::Delivery(delivery_error),
                ),
            );
            assert!(scoped_source_owner_error_is_delivery_backpressure(&error));
            assert!(!scoped_source_owner_error_is_transient(&error));
            assert!(!scoped_source_owner_error_is_cancelled(&error));
        }
        let oversized = ScopedObservationPassExecutionError::Offer(
            ScopedObservationConsumerOfferError::Offer(ScopedProjectionDeliveryError::Delivery(
                ScopedDeliveryError::SourceControlBatchTooLarge,
            )),
        );
        assert!(!scoped_source_owner_error_is_delivery_backpressure(
            &oversized
        ));
        let admission_full =
            ScopedObservationPassExecutionError::Admission(ScopedAdmissionError::DataQueueFull);
        assert!(!scoped_source_owner_error_is_delivery_backpressure(
            &admission_full
        ));
        assert!(!scoped_source_owner_error_is_transient(&admission_full));

        for transient in [
            ScopedObservationPassExecutionError::DecodeRetryTransient,
            ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Decode(
                ScopedDecodeFailureClass::Transient,
            )),
            ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Source(
                ScopedSourceFailureClass::Unstable,
            )),
            ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Source(
                ScopedSourceFailureClass::Database,
            )),
            ScopedObservationPassExecutionError::Access(ScopedObservationAccessError::Source(
                ScopedSourceFailureClass::Io,
            )),
        ] {
            assert!(scoped_source_owner_error_is_transient(&transient));
            assert!(!scoped_source_owner_error_is_delivery_backpressure(
                &transient
            ));
            assert!(matches!(
                scoped_object_failure_classification(&transient),
                Some(ScopedObjectFailureClassification::Retryable(_))
            ));
        }
        assert_eq!(
            scoped_object_failure_classification(&ScopedObservationPassExecutionError::Access(
                ScopedObservationAccessError::Decode(ScopedDecodeFailureClass::StreamFatal),
            ),),
            Some(ScopedObjectFailureClassification::Terminal(
                ScopedSourceObjectFailureCode::DecodeStreamFatal,
            ))
        );
        for attachment_failure in [
            ScopedDecodeFailureClass::AdapterFatal,
            ScopedDecodeFailureClass::InvalidContract,
        ] {
            assert_eq!(
                scoped_object_failure_classification(&ScopedObservationPassExecutionError::Access(
                    ScopedObservationAccessError::Decode(attachment_failure),
                ),),
                None
            );
        }
        let cancelled =
            ScopedObservationPassExecutionError::Offer(ScopedObservationConsumerOfferError::Closed);
        assert!(scoped_source_owner_error_is_cancelled(&cancelled));
    }

    #[tokio::test]
    async fn scoped_delivery_capacity_wakeup_is_retained_and_closes() {
        let mut delivery = delivery_lane(1, 1);
        let source = source_identity();
        let created = ScopedAppendPresenceChange::Created { generation: 1 };
        delivery
            .offer(vec![ScopedProjectedObservation::SourcePresence {
                object_token: OBJECT_TOKEN,
                source: source.clone(),
                lane_ordinal: 1,
                observed_at: 10,
                phase: ScopedAppendDeliveryPhase::Bootstrap,
                event_id: source_presence_event_id(&source, created),
                change: created,
            }])
            .unwrap();
        let waiter = delivery.capacity_waiter();
        let before = waiter.snapshot();
        assert_eq!(before.generation, 0);
        assert!(!before.closed);

        // Release before the future is constructed/first-polled. The retained
        // generation still makes the waiter complete immediately.
        assert!(delivery.pop_next().is_some());
        let released = tokio::time::timeout(
            Duration::from_secs(1),
            waiter.wait_after_async(before.generation),
        )
        .await
        .unwrap();
        assert_eq!(released.generation, 1);
        assert!(!released.closed);

        let close_generation = released.generation;
        delivery.discard_for_close();
        let closed = waiter.wait_after_async(close_generation).await;
        assert_eq!(closed.generation, close_generation);
        assert!(closed.closed);
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
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), false, 20)
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
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), true, 20)
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
    fn scoped_reoverflow_discards_incomplete_stage_and_requires_a_fresh_epoch() {
        let root = root_identity();
        let mapper = ScopedObservationEnvelopeMapper::new(root.clone());
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
        let bootstrap = delivery
            .offer_bootstrap_barrier(&root, watermark, Vec::new(), true, 20)
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 1);
        delivery
            .require_resync(&root, ScopedResyncReason::WatcherOverflow, 30)
            .unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 2);
        delivery.begin_resync(&root, 40).unwrap();
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 3);

        let replay_record = record(1, 10, 20);
        let replay_frame = decoded_frame(
            2,
            ScopedAppendDeliveryPhase::Bootstrap,
            &replay_record,
            usage_batch(&replay_record, "replacement-response", 20, None),
        );
        let mut replay = admission_lane_with_decoded_frame(replay_frame);
        let mut stale_stage =
            ScopedObservationReplacementStage::new(root.clone(), 2, active.limits).unwrap();
        assert!(stale_stage.reduce_next(&mut replay).unwrap());
        stale_stage.prepare_snapshot(&replay, &delivery).unwrap();
        let receipt = stale_stage
            .offer_snapshot_next(&mut delivery)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.first_offered_sequence, Some(4));
        assert_eq!(delivery.queued_semantic_events(), 1);

        let reoverflow = delivery
            .require_resync(&root, ScopedResyncReason::TransportContinuityLoss, 50)
            .unwrap();
        assert_eq!(reoverflow.invalid_scope_epoch, 2);
        assert_eq!(reoverflow.control_sequence, 5);
        assert_eq!(reoverflow.last_contiguous_sequence, 3);
        assert_eq!(
            reoverflow.baseline_snapshot_digest,
            bootstrap.replacement_snapshot_digest
        );
        assert_eq!(reoverflow.discarded_semantic_events, 1);
        assert_eq!(reoverflow.discarded_source_controls, 0);
        assert_eq!(delivery.queued_semantic_events(), 0);
        assert_eq!(delivery.queued_source_control_items(), 1);
        assert_eq!(
            delivery.state().continuity,
            ScopedObservationContinuity::ResyncRequired
        );
        assert_eq!(
            stale_stage.offer_snapshot_next(&mut delivery),
            Err(ScopedReplacementStageError::NotResyncing)
        );

        let active_snapshot = active
            .usage_v2_replacement_snapshot(ScopedAppendDeliveryPhase::Correction)
            .unwrap();
        assert_eq!(active_snapshot.entity_count, 1);
        assert_eq!(
            active_snapshot.events[0].revision.response_key,
            b"active-response"
        );
        let repeated = delivery
            .require_resync(&root, ScopedResyncReason::ExplicitConsumerRequest, 999)
            .unwrap();
        assert!(Arc::ptr_eq(&reoverflow, &repeated));

        let delivered = delivery.pop_next().unwrap();
        assert_eq!(delivered.scope_epoch, 2);
        assert_eq!(delivered.observer_sequence, 5);
        assert!(matches!(
            mapper.map(delivered).unwrap().event,
            ScopedObservationEvent::ObserverResyncRequired {
                control: delivered_control,
            } if Arc::ptr_eq(&reoverflow, &delivered_control)
        ));
        let restarted = delivery.begin_resync(&root, 60).unwrap();
        assert_eq!(restarted.old_scope_epoch, 2);
        assert_eq!(restarted.new_scope_epoch, 3);
        assert_eq!(restarted.control_sequence, 6);
        assert_eq!(restarted.required_control_sequence, 5);
        assert_eq!(delivery.state().scope_epoch, 3);
        assert_eq!(
            stale_stage.offer_snapshot_next(&mut delivery),
            Err(ScopedReplacementStageError::EpochMismatch)
        );
        let fresh_stage =
            ScopedObservationReplacementStage::new(root.clone(), 3, active.limits).unwrap();
        assert_eq!(fresh_stage.usage_v2_entity_count(), 0);
        assert_eq!(delivery.pop_next().unwrap().observer_sequence, 6);
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
        let event_waiter = delivery.event_waiter();
        let before_repeat = event_waiter.snapshot();
        assert_eq!(before_repeat.offered_through_sequence, 1);

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
        assert_eq!(event_waiter.snapshot(), before_repeat);

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
