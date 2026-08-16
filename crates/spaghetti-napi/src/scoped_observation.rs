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
    AdapterError, AdapterErrorClass, AdapterId, AdapterObjectContext, AdapterRegistry,
    AgentAdapter, CompatibilityDecision, ContractVersionOffer, ContractVersionRequest,
    DecodeDisposition, DecoderId, Fact, FactBatch, NativeArtifactProbe, RawRetentionPolicy,
    ScopeRelationPrimitive, SourceAccess, SourceObjectList, SourceObjectListRequest, SourceQuery,
    SourceRows, SourceSnapshot, SupportOperation, TypedAccessAuthorization,
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
    pub program_id: String,
    pub known_objects: Vec<ScopedKnownObjectGrant>,
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

#[derive(Debug, PartialEq, Eq)]
pub struct ScopedAppendObservation {
    object_token: u64,
    admission_token: u64,
    pub phase: ScopedAppendDeliveryPhase,
    pub reset_before_items: Option<ScopedAppendReset>,
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
    Reset {
        lane_ordinal: u64,
        phase: ScopedAppendDeliveryPhase,
        reset: ScopedAppendReset,
    },
    Decoded {
        lane_ordinal: u64,
        phase: ScopedAppendDeliveryPhase,
        item: ScopedDecodedAppendItem,
    },
}

impl ScopedQueuedObservationFrame {
    pub fn lane_ordinal(&self) -> u64 {
        match self {
            Self::Reset { lane_ordinal, .. } | Self::Decoded { lane_ordinal, .. } => *lane_ordinal,
        }
    }
}

struct QueuedDecodedFrame {
    lane_ordinal: u64,
    phase: ScopedAppendDeliveryPhase,
    item: ScopedDecodedAppendItem,
    data_events: u64,
    retained_native_bytes: u64,
}

struct QueuedResetFrame {
    lane_ordinal: u64,
    phase: ScopedAppendDeliveryPhase,
    reset: ScopedAppendReset,
}

/// Two bounded internal capacity domains multiplexed by one admission ordinal.
/// The ordinal is not an RFC 012D `observer_sequence`; public sequencing begins
/// only after canonical semantic projection is available.
pub struct ScopedObservationAdmissionLane {
    limits: ScopedObservationQueueLimits,
    decoded: VecDeque<QueuedDecodedFrame>,
    controls: VecDeque<QueuedResetFrame>,
    queued_data_events: u64,
    queued_retained_native_bytes: u64,
    next_lane_ordinal: u64,
}

impl ScopedObservationAdmissionLane {
    pub fn new(limits: ScopedObservationQueueLimits) -> Result<Self, ScopedAdmissionError> {
        if limits.max_data_events == 0 || limits.max_control_items == 0 {
            return Err(ScopedAdmissionError::InvalidLimits);
        }
        Ok(Self {
            limits,
            decoded: VecDeque::new(),
            controls: VecDeque::new(),
            queued_data_events: 0,
            queued_retained_native_bytes: 0,
            next_lane_ordinal: 1,
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
        let control_items = usize::from(observation.reset_before_items.is_some());
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
        if let Some(reset) = observation.reset_before_items {
            self.controls.push_back(QueuedResetFrame {
                lane_ordinal,
                phase: observation.phase,
                reset,
            });
            lane_ordinal += 1;
        }
        for (item, (item_events, item_bytes)) in decoded.items.drain(..).zip(measurements) {
            self.decoded.push_back(QueuedDecodedFrame {
                lane_ordinal,
                phase: observation.phase,
                item,
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

    pub fn pop_next(&mut self) -> Option<ScopedQueuedObservationFrame> {
        let take_control = match (self.controls.front(), self.decoded.front()) {
            (Some(control), Some(decoded)) => control.lane_ordinal < decoded.lane_ordinal,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return None,
        };
        if take_control {
            let control = self.controls.pop_front().expect("control front exists");
            return Some(ScopedQueuedObservationFrame::Reset {
                lane_ordinal: control.lane_ordinal,
                phase: control.phase,
                reset: control.reset,
            });
        }
        let decoded = self.decoded.pop_front().expect("decoded front exists");
        self.queued_data_events = self
            .queued_data_events
            .checked_sub(decoded.data_events)
            .expect("queued decoded event accounting cannot underflow");
        self.queued_retained_native_bytes = self
            .queued_retained_native_bytes
            .checked_sub(decoded.retained_native_bytes)
            .expect("queued retained-native accounting cannot underflow");
        Some(ScopedQueuedObservationFrame::Decoded {
            lane_ordinal: decoded.lane_ordinal,
            phase: decoded.phase,
            item: decoded.item,
        })
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

    pub fn is_empty(&self) -> bool {
        self.controls.is_empty() && self.decoded.is_empty()
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
        let program = authorization
            .select_scope_program(&request.program_id)
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program)?;
        let known_objects = validate_known_object_grants(&plan, request.known_objects)?;
        Ok(Self {
            adapter,
            compatibility,
            authorization,
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
    state: Arc<ScopedObservationAccessState>,
    released: bool,
}

impl ScopedObservationAccessPass {
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
        decoder: ScopedAppendDecoderConfig,
    ) -> Result<Self, ScopedObservationAccessError> {
        validate_decode_bounds(&decoder)?;
        let object_token = next_scoped_object_token()?;
        Ok(Self {
            object_token,
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
            became_missing,
            next_checkpoint,
            next_root_present,
            next_bootstrap_blocked,
        ) = match &read {
            AppendRead::Missing => {
                let became_missing = self.root_present;
                (None, became_missing, self.checkpoint.clone(), false, false)
            }
            AppendRead::RetryTransient => (
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
                (
                    reset,
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
            phase,
            reset_before_items,
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
