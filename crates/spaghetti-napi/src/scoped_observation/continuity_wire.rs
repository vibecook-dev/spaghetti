//! Strict portable RFC 012D wire projection for continuity invalidation,
//! replacement start, and terminal observer failure controls.
//!
//! Bootstrap and replacement-completion barriers use their separate strict
//! completion contract. This top-level value intentionally has no unbound
//! `Deserialize` path: consumption requires caller-held negotiation, root, and
//! lifecycle state. Rust derives the observer-control coordinate and
//! recomputes every deterministic event ID.

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{
    ActorRunRole, CanonicalEntityKey, CanonicalSourceInstanceKey, ContractCompleteness,
    CoverageObjectKey, CoverageStreamKey, ExternalEntityRef, NativeIdentityClaim,
    QualifiedTimestamp, QualifiedValueQuality, SemanticRevisionRef,
};
use crate::observation_contract::ObservationContractSelection;

use super::{
    observer_control_source, observer_failed_event_id, resync_required_event_id,
    resync_started_event_id, valid_replacement_baseline_snapshot_digest, ScopedActorAttribution,
    ScopedActorFallbackReason, ScopedAppendDeliveryPhase, ScopedEnvelopeEvidenceAuthority,
    ScopedNativeEvidence, ScopedObservationAttachmentAuthority, ScopedObservationContinuity,
    ScopedObservationDeliveryLane, ScopedObservationEnvelope, ScopedObservationEvent,
    ScopedObservationRootIdentity, ScopedObserverFailureReason, ScopedReplacementMode,
    ScopedReplacementSnapshotDigest, ScopedResyncReason, ScopedResyncRequired, ScopedResyncStarted,
    SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION,
};

pub(crate) const SCOPED_CONTINUITY_ENVELOPE_CONTRACT_VERSION: u32 = 1;

const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedContinuityEnvelopeContractError {
    #[error("invalid scoped continuity envelope contract: {message}")]
    Invalid { message: String },
    #[error("scoped envelope event is not supported by the continuity wire contract")]
    UnsupportedEvent,
}

impl ScopedContinuityEnvelopeContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

/// State retained independently by the sole ordered consumer. A received
/// control cannot establish its own current epoch, applied watermark, baseline
/// snapshot, or delivered invalidation lineage.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedContinuityConsumerContext {
    pub current_scope_epoch: u64,
    pub last_contiguous_sequence: u64,
    pub baseline_snapshot_digest: Option<ScopedReplacementSnapshotDigest>,
    pub phase: ScopedAppendDeliveryPhase,
    pub prior_resync_required: Option<ScopedResyncRequired>,
}

impl std::fmt::Debug for ScopedContinuityConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedContinuityConsumerContext")
            .field("current_scope_epoch", &self.current_scope_epoch)
            .field("last_contiguous_sequence", &self.last_contiguous_sequence)
            .field(
                "has_completed_baseline",
                &self.baseline_snapshot_digest.is_some(),
            )
            .field("phase", &self.phase)
            .field(
                "has_prior_resync_required",
                &self.prior_resync_required.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// Exact process-local authority retained beside one yielded continuity
/// control. The state is derived from the delivery lane before dequeue rather
/// than from fields supplied by the received control.
#[derive(Clone)]
pub(crate) struct ScopedContinuityEnvelopeConsumerContext {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    contract_selection: ObservationContractSelection,
    root: ScopedObservationRootIdentity,
    state: ScopedContinuityConsumerContext,
}

impl std::fmt::Debug for ScopedContinuityEnvelopeConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedContinuityEnvelopeConsumerContext")
            .field("current_scope_epoch", &self.state.current_scope_epoch)
            .field(
                "last_contiguous_sequence",
                &self.state.last_contiguous_sequence,
            )
            .field(
                "has_completed_baseline",
                &self.state.baseline_snapshot_digest.is_some(),
            )
            .field("phase", &self.state.phase)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContinuityDeliveryPhaseWire {
    Bootstrap,
    Live,
    Correction,
}

impl From<ScopedAppendDeliveryPhase> for ContinuityDeliveryPhaseWire {
    fn from(value: ScopedAppendDeliveryPhase) -> Self {
        match value {
            ScopedAppendDeliveryPhase::Bootstrap => Self::Bootstrap,
            ScopedAppendDeliveryPhase::Live => Self::Live,
            ScopedAppendDeliveryPhase::Correction => Self::Correction,
        }
    }
}

impl From<ContinuityDeliveryPhaseWire> for ScopedAppendDeliveryPhase {
    fn from(value: ContinuityDeliveryPhaseWire) -> Self {
        match value {
            ContinuityDeliveryPhaseWire::Bootstrap => Self::Bootstrap,
            ContinuityDeliveryPhaseWire::Live => Self::Live,
            ContinuityDeliveryPhaseWire::Correction => Self::Correction,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResyncReasonWire {
    WatcherOverflow,
    TransportContinuityLoss,
    ExplicitConsumerRequest,
}

impl From<ScopedResyncReason> for ResyncReasonWire {
    fn from(value: ScopedResyncReason) -> Self {
        match value {
            ScopedResyncReason::WatcherOverflow => Self::WatcherOverflow,
            ScopedResyncReason::TransportContinuityLoss => Self::TransportContinuityLoss,
            ScopedResyncReason::ExplicitConsumerRequest => Self::ExplicitConsumerRequest,
        }
    }
}

impl From<ResyncReasonWire> for ScopedResyncReason {
    fn from(value: ResyncReasonWire) -> Self {
        match value {
            ResyncReasonWire::WatcherOverflow => Self::WatcherOverflow,
            ResyncReasonWire::TransportContinuityLoss => Self::TransportContinuityLoss,
            ResyncReasonWire::ExplicitConsumerRequest => Self::ExplicitConsumerRequest,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ObserverFailureReasonWire {
    NativeWatcherRecoveryExhausted,
    NativeWatcherRoutingFailed,
    InternalControlFailure,
}

impl From<ScopedObserverFailureReason> for ObserverFailureReasonWire {
    fn from(value: ScopedObserverFailureReason) -> Self {
        match value {
            ScopedObserverFailureReason::NativeWatcherRecoveryExhausted => {
                Self::NativeWatcherRecoveryExhausted
            }
            ScopedObserverFailureReason::NativeWatcherRoutingFailed => {
                Self::NativeWatcherRoutingFailed
            }
            ScopedObserverFailureReason::InternalControlFailure => Self::InternalControlFailure,
        }
    }
}

impl From<ObserverFailureReasonWire> for ScopedObserverFailureReason {
    fn from(value: ObserverFailureReasonWire) -> Self {
        match value {
            ObserverFailureReasonWire::NativeWatcherRecoveryExhausted => {
                Self::NativeWatcherRecoveryExhausted
            }
            ObserverFailureReasonWire::NativeWatcherRoutingFailed => {
                Self::NativeWatcherRoutingFailed
            }
            ObserverFailureReasonWire::InternalControlFailure => Self::InternalControlFailure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReplacementModeWire {
    FullSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuityRootWire {
    session_ref: ExternalEntityRef,
    session_key: CanonicalEntityKey,
    root_actor_run_key: CanonicalEntityKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_claim: Option<NativeIdentityClaim>,
}

impl ContinuityRootWire {
    fn from_root(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            session_ref: root.session_ref,
            session_key: root.session_key,
            root_actor_run_key: root.root_actor_run_key,
            native_session_claim: root.native_session_claim.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuityActorWire {
    root_session_key: CanonicalEntityKey,
    run_key: CanonicalEntityKey,
    role: ContinuityActorRoleWire,
    #[serde(deserialize_with = "deserialize_required_option")]
    parent_run_key: Option<CanonicalEntityKey>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_actor_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_actor_type: Option<String>,
}

impl ContinuityActorWire {
    fn from_root(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            root_session_key: root.session_key,
            run_key: root.root_actor_run_key,
            role: ContinuityActorRoleWire::Root,
            parent_run_key: None,
            native_session_id: root
                .native_session_claim
                .as_ref()
                .and_then(|claim| claim.identity.value.as_ref())
                .map(|identity| identity.native_id.clone()),
            native_actor_id: None,
            native_actor_type: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContinuityActorRoleWire {
    Root,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuityActorAttributionWire {
    kind: String,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuityAffiliationsWire {
    actor_run_key: CanonicalEntityKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    team_key: Option<CanonicalEntityKey>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_team_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    team_name: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    member_key: Option<CanonicalEntityKey>,
    #[serde(deserialize_with = "deserialize_required_option")]
    workflow_key: Option<CanonicalEntityKey>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_workflow_id: Option<String>,
    completeness: ContractCompleteness,
    derived_from_revision_refs: Vec<SemanticRevisionRef>,
}

impl ContinuityAffiliationsWire {
    fn from_root(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            actor_run_key: root.root_actor_run_key,
            team_key: None,
            native_team_id: None,
            team_name: None,
            member_key: None,
            workflow_key: None,
            native_workflow_id: None,
            completeness: ContractCompleteness::Unknown,
            derived_from_revision_refs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuitySourceWire {
    instance_key: CanonicalSourceInstanceKey,
    stream_key: CoverageStreamKey,
    object_key: CoverageObjectKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    locator_id: Option<String>,
    generation: u64,
    #[serde(deserialize_with = "deserialize_required_option")]
    source_record_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    record_index: Option<u32>,
    #[serde(deserialize_with = "deserialize_required_option")]
    cursor_start: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    cursor_end: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    byte_range: Option<JsonValue>,
}

impl ContinuitySourceWire {
    fn from_root(
        root: &ScopedObservationRootIdentity,
        generation: u64,
    ) -> Result<Self, ScopedContinuityEnvelopeContractError> {
        let source = observer_control_source(root).map_err(|()| {
            ScopedContinuityEnvelopeContractError::invalid(
                "observer control source identity cannot be derived",
            )
        })?;
        Ok(Self {
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
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuityEvidenceWire {
    authority: ContinuityEvidenceAuthorityWire,
    quality: QualifiedValueQuality,
    #[serde(deserialize_with = "deserialize_required_option")]
    effective_at: Option<QualifiedTimestamp>,
    completeness: ContractCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContinuityEvidenceAuthorityWire {
    EngineControl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuityNativeEvidenceWire {
    kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum ContinuityControlEventWire {
    #[serde(rename = "observer_resync_required")]
    ResyncRequired {
        invalid_scope_epoch: u64,
        control_sequence: u64,
        last_contiguous_sequence: u64,
        baseline_snapshot_digest: String,
        reason: ResyncReasonWire,
        discarded_semantic_events: u64,
        discarded_source_controls: u64,
        discarded_retained_native_bytes: u64,
    },
    #[serde(rename = "observer_resync_started")]
    ResyncStarted {
        old_scope_epoch: u64,
        new_scope_epoch: u64,
        control_sequence: u64,
        required_control_sequence: u64,
        baseline_snapshot_digest: String,
        reason: ResyncReasonWire,
        replacement: ReplacementModeWire,
    },
    #[serde(rename = "observer_failed")]
    Failed {
        failed_scope_epoch: u64,
        control_sequence: u64,
        last_contiguous_sequence: u64,
        phase: ContinuityDeliveryPhaseWire,
        reason: ObserverFailureReasonWire,
        discarded_semantic_events: u64,
        discarded_source_controls: u64,
        discarded_retained_native_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ContinuityControlSourceBindingWire {
    instance_key: CanonicalSourceInstanceKey,
    stream_key: CoverageStreamKey,
    object_key: CoverageObjectKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ContinuityPriorResyncRequiredWire {
    kind: &'static str,
    invalid_scope_epoch: u64,
    control_sequence: u64,
    last_contiguous_sequence: u64,
    baseline_snapshot_digest: String,
    reason: ResyncReasonWire,
    discarded_semantic_events: u64,
    discarded_source_controls: u64,
    discarded_retained_native_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ContinuityConsumerStateWire {
    current_scope_epoch: u64,
    last_contiguous_sequence: u64,
    baseline_snapshot_digest: Option<String>,
    phase: ContinuityDeliveryPhaseWire,
    prior_resync_required: Option<ContinuityPriorResyncRequiredWire>,
}

/// Serialize-only caller context consumed by the existing strict portable
/// continuity parser. The process-local attachment authority remains outside
/// this value and cannot be reconstructed from JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopedContinuityEnvelopeContextWire {
    contract_selection: ObservationContractSelection,
    root: ContinuityRootWire,
    control_source: ContinuityControlSourceBindingWire,
    state: ContinuityConsumerStateWire,
}

impl ScopedContinuityEnvelopeConsumerContext {
    pub(super) fn from_delivered(
        envelope: &ScopedObservationEnvelope,
        expected_root: &ScopedObservationRootIdentity,
        attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
        delivery: &ScopedObservationDeliveryLane,
    ) -> Result<Option<Self>, ScopedContinuityEnvelopeContractError> {
        let delivery_state = delivery.state();
        let state = match &envelope.event {
            ScopedObservationEvent::ObserverResyncRequired { control } => {
                let retained = delivery.resync_required().ok_or_else(|| {
                    ScopedContinuityEnvelopeContractError::invalid(
                        "resync-required delivery is missing its retained control",
                    )
                })?;
                let baseline =
                    valid_replacement_baseline_snapshot_digest(delivery).ok_or_else(|| {
                        ScopedContinuityEnvelopeContractError::invalid(
                            "resync-required delivery has no completed baseline",
                        )
                    })?;
                if !Arc::ptr_eq(control, &retained)
                    || delivery_state.continuity != ScopedObservationContinuity::ResyncRequired
                    || delivery_state.scope_epoch != control.invalid_scope_epoch
                    || delivery_state.delivered_through_sequence != control.last_contiguous_sequence
                    || baseline != control.baseline_snapshot_digest
                {
                    return Err(ScopedContinuityEnvelopeContractError::invalid(
                        "resync-required delivery does not match retained lane state",
                    ));
                }
                ScopedContinuityConsumerContext {
                    current_scope_epoch: control.invalid_scope_epoch,
                    last_contiguous_sequence: delivery_state.delivered_through_sequence,
                    baseline_snapshot_digest: Some(baseline),
                    phase: ScopedAppendDeliveryPhase::Live,
                    prior_resync_required: None,
                }
            }
            ScopedObservationEvent::ObserverResyncStarted { control } => {
                let retained = delivery.resync_started().ok_or_else(|| {
                    ScopedContinuityEnvelopeContractError::invalid(
                        "resync-started delivery is missing its retained control",
                    )
                })?;
                let required = delivery.resync_required().ok_or_else(|| {
                    ScopedContinuityEnvelopeContractError::invalid(
                        "resync-started delivery is missing its prior invalidation",
                    )
                })?;
                if !Arc::ptr_eq(control, &retained)
                    || delivery_state.continuity != ScopedObservationContinuity::Resyncing
                    || delivery_state.scope_epoch != control.new_scope_epoch
                    || delivery_state.delivered_through_sequence != required.control_sequence
                    || required.invalid_scope_epoch != control.old_scope_epoch
                    || required.control_sequence != control.required_control_sequence
                    || required.baseline_snapshot_digest != control.baseline_snapshot_digest
                    || required.reason != control.reason
                {
                    return Err(ScopedContinuityEnvelopeContractError::invalid(
                        "resync-started delivery does not match retained lane state",
                    ));
                }
                ScopedContinuityConsumerContext {
                    current_scope_epoch: control.old_scope_epoch,
                    last_contiguous_sequence: delivery_state.delivered_through_sequence,
                    baseline_snapshot_digest: Some(required.baseline_snapshot_digest),
                    phase: ScopedAppendDeliveryPhase::Live,
                    prior_resync_required: Some((*required).clone()),
                }
            }
            ScopedObservationEvent::ObserverFailed { failure } => {
                let retained = delivery.observer_failure().ok_or_else(|| {
                    ScopedContinuityEnvelopeContractError::invalid(
                        "observer-failed delivery is missing its retained control",
                    )
                })?;
                if !Arc::ptr_eq(failure, &retained)
                    || delivery_state.continuity != ScopedObservationContinuity::Failed
                    || delivery_state.scope_epoch != failure.failed_scope_epoch
                    || delivery_state.delivered_through_sequence != failure.last_contiguous_sequence
                {
                    return Err(ScopedContinuityEnvelopeContractError::invalid(
                        "observer-failed delivery does not match retained lane state",
                    ));
                }
                ScopedContinuityConsumerContext {
                    current_scope_epoch: failure.failed_scope_epoch,
                    last_contiguous_sequence: delivery_state.delivered_through_sequence,
                    baseline_snapshot_digest: valid_replacement_baseline_snapshot_digest(delivery),
                    phase: failure.phase,
                    prior_resync_required: None,
                }
            }
            ScopedObservationEvent::SourcePresence { .. }
            | ScopedObservationEvent::SourceReset { .. }
            | ScopedObservationEvent::SourceObjectError { .. }
            | ScopedObservationEvent::UsageV2 { .. }
            | ScopedObservationEvent::ActorRun { .. }
            | ScopedObservationEvent::ActorAffiliation { .. }
            | ScopedObservationEvent::ArtifactAvailability { .. }
            | ScopedObservationEvent::UnknownWire { .. }
            | ScopedObservationEvent::ObserverBootstrapComplete { .. }
            | ScopedObservationEvent::ObserverResyncComplete { .. } => return Ok(None),
        };
        ScopedContinuityEnvelopeWire::from_scoped_for_context(
            envelope,
            &envelope.contract_selection,
            expected_root,
            &state,
        )?;
        let context = Self {
            attachment_authority,
            contract_selection: envelope.contract_selection.clone(),
            root: expected_root.clone(),
            state,
        };
        context.wire()?;
        Ok(Some(context))
    }

    pub(crate) fn state(&self) -> &ScopedContinuityConsumerContext {
        &self.state
    }

    pub(crate) fn wire(
        &self,
    ) -> Result<ScopedContinuityEnvelopeContextWire, ScopedContinuityEnvelopeContractError> {
        require_positive_safe(self.state.current_scope_epoch, "caller current_scope_epoch")?;
        require_safe_u64(
            self.state.last_contiguous_sequence,
            "caller last_contiguous_sequence",
        )?;
        validate_context_baseline(&self.state)?;
        let prior_resync_required = self
            .state
            .prior_resync_required
            .as_ref()
            .map(|required| {
                validate_prior_required(required, &self.root, &self.state)?;
                Ok(ContinuityPriorResyncRequiredWire {
                    kind: "observer_resync_required",
                    invalid_scope_epoch: required.invalid_scope_epoch,
                    control_sequence: required.control_sequence,
                    last_contiguous_sequence: required.last_contiguous_sequence,
                    baseline_snapshot_digest: encode_opaque(
                        required.baseline_snapshot_digest.as_bytes(),
                    ),
                    reason: required.reason.into(),
                    discarded_semantic_events: required.discarded_semantic_events,
                    discarded_source_controls: required.discarded_source_controls,
                    discarded_retained_native_bytes: required.discarded_retained_native_bytes,
                })
            })
            .transpose()?;
        let control_source = observer_control_source(&self.root).map_err(|()| {
            ScopedContinuityEnvelopeContractError::invalid(
                "observer control source identity cannot be derived",
            )
        })?;
        Ok(ScopedContinuityEnvelopeContextWire {
            contract_selection: self.contract_selection.clone(),
            root: ContinuityRootWire::from_root(&self.root),
            control_source: ContinuityControlSourceBindingWire {
                instance_key: control_source.source_instance_key,
                stream_key: control_source.stream_key,
                object_key: control_source.object_key,
            },
            state: ContinuityConsumerStateWire {
                current_scope_epoch: self.state.current_scope_epoch,
                last_contiguous_sequence: self.state.last_contiguous_sequence,
                baseline_snapshot_digest: self
                    .state
                    .baseline_snapshot_digest
                    .map(|digest| encode_opaque(digest.as_bytes())),
                phase: self.state.phase.into(),
                prior_resync_required,
            },
        })
    }

    pub(crate) fn wire_value(&self) -> Result<JsonValue, ScopedContinuityEnvelopeContractError> {
        serde_json::to_value(self.wire()?)
            .map_err(|error| ScopedContinuityEnvelopeContractError::invalid(error.to_string()))
    }

    pub(super) fn belongs_to_attachment(
        &self,
        authority: &Arc<ScopedObservationAttachmentAuthority>,
    ) -> bool {
        Arc::ptr_eq(&self.attachment_authority, authority)
    }
}

/// Specialized serialization-only projection of three observer controls.
/// Received values must use `from_wire_value_for_context`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedContinuityEnvelopeWire {
    scoped_continuity_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: ObservationContractSelection,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: Option<SemanticRevisionRef>,
    root: ContinuityRootWire,
    actor: ContinuityActorWire,
    actor_attribution: ContinuityActorAttributionWire,
    affiliations: ContinuityAffiliationsWire,
    source: ContinuitySourceWire,
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: ContinuityDeliveryPhaseWire,
    evidence: ContinuityEvidenceWire,
    event: ContinuityControlEventWire,
    native_evidence: ContinuityNativeEvidenceWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedContinuityEnvelopeInput {
    scoped_continuity_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: JsonValue,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    semantic_revision_ref: Option<SemanticRevisionRef>,
    root: ContinuityRootWire,
    actor: ContinuityActorWire,
    actor_attribution: ContinuityActorAttributionWire,
    affiliations: ContinuityAffiliationsWire,
    source: ContinuitySourceWire,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: ContinuityDeliveryPhaseWire,
    evidence: ContinuityEvidenceWire,
    event: ContinuityControlEventWire,
    native_evidence: ContinuityNativeEvidenceWire,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl ScopedContinuityEnvelopeWire {
    pub(crate) fn from_scoped_for_context(
        envelope: &ScopedObservationEnvelope,
        expected_selection: &ObservationContractSelection,
        expected_root: &ScopedObservationRootIdentity,
        expected_state: &ScopedContinuityConsumerContext,
    ) -> Result<Self, ScopedContinuityEnvelopeContractError> {
        let event = match &envelope.event {
            ScopedObservationEvent::ObserverResyncRequired { control } => {
                if control.root != *expected_root {
                    return Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent);
                }
                ContinuityControlEventWire::ResyncRequired {
                    invalid_scope_epoch: control.invalid_scope_epoch,
                    control_sequence: control.control_sequence,
                    last_contiguous_sequence: control.last_contiguous_sequence,
                    baseline_snapshot_digest: encode_opaque(
                        control.baseline_snapshot_digest.as_bytes(),
                    ),
                    reason: control.reason.into(),
                    discarded_semantic_events: control.discarded_semantic_events,
                    discarded_source_controls: control.discarded_source_controls,
                    discarded_retained_native_bytes: control.discarded_retained_native_bytes,
                }
            }
            ScopedObservationEvent::ObserverResyncStarted { control } => {
                if control.root != *expected_root {
                    return Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent);
                }
                ContinuityControlEventWire::ResyncStarted {
                    old_scope_epoch: control.old_scope_epoch,
                    new_scope_epoch: control.new_scope_epoch,
                    control_sequence: control.control_sequence,
                    required_control_sequence: control.required_control_sequence,
                    baseline_snapshot_digest: encode_opaque(
                        control.baseline_snapshot_digest.as_bytes(),
                    ),
                    reason: control.reason.into(),
                    replacement: ReplacementModeWire::FullSnapshot,
                }
            }
            ScopedObservationEvent::ObserverFailed { failure } => {
                if failure.root != *expected_root {
                    return Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent);
                }
                ContinuityControlEventWire::Failed {
                    failed_scope_epoch: failure.failed_scope_epoch,
                    control_sequence: failure.control_sequence,
                    last_contiguous_sequence: failure.last_contiguous_sequence,
                    phase: failure.phase.into(),
                    reason: failure.reason.into(),
                    discarded_semantic_events: failure.discarded_semantic_events,
                    discarded_source_controls: failure.discarded_source_controls,
                    discarded_retained_native_bytes: failure.discarded_retained_native_bytes,
                }
            }
            _ => return Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent),
        };
        if envelope.semantic_revision_ref.is_some()
            || envelope.native_time.is_some()
            || envelope.source.locator_id.is_some()
            || envelope.source.source_record_id.is_some()
            || envelope.source.record_index.is_some()
            || envelope.source.cursor_start.is_some()
            || envelope.source.cursor_end.is_some()
            || envelope.source.byte_range.is_some()
            || envelope.evidence.authority != ScopedEnvelopeEvidenceAuthority::EngineControl
            || envelope.evidence.quality != QualifiedValueQuality::Derived
            || envelope.evidence.effective_at.is_some()
            || envelope.evidence.completeness != ContractCompleteness::Complete
            || !matches!(
                envelope.native_evidence,
                ScopedNativeEvidence::EngineControl
            )
            || envelope.actor_attribution
                != (ScopedActorAttribution::ScopeFallback {
                    reason: ScopedActorFallbackReason::ObserverLifecycleControl,
                })
        {
            return Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent);
        }
        let value = Self {
            scoped_continuity_envelope_contract_version:
                SCOPED_CONTINUITY_ENVELOPE_CONTRACT_VERSION,
            contract_version: envelope.contract_version,
            contract_selection: envelope.contract_selection.clone(),
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
            event_id: encode_opaque(envelope.event_id.as_bytes()),
            semantic_revision_ref: None,
            root: ContinuityRootWire {
                session_ref: envelope.root.session_ref,
                session_key: envelope.root.session_key,
                root_actor_run_key: envelope.root.root_actor_run_key,
                native_session_claim: envelope.root.native_session_claim.clone(),
            },
            actor: ContinuityActorWire {
                root_session_key: envelope.actor.root_session_key,
                run_key: envelope.actor.run_key,
                role: match envelope.actor.role {
                    ActorRunRole::Root => ContinuityActorRoleWire::Root,
                    ActorRunRole::Child => {
                        return Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent)
                    }
                },
                parent_run_key: envelope.actor.parent_run_key,
                native_session_id: envelope.actor.native_session_id.clone(),
                native_actor_id: envelope.actor.native_actor_id.clone(),
                native_actor_type: envelope.actor.native_actor_type.clone(),
            },
            actor_attribution: ContinuityActorAttributionWire {
                kind: "scope_fallback".to_owned(),
                reason: "observer_lifecycle_control".to_owned(),
            },
            affiliations: ContinuityAffiliationsWire {
                actor_run_key: envelope.affiliations.actor_run_key,
                team_key: envelope.affiliations.team_key,
                native_team_id: envelope.affiliations.native_team_id.clone(),
                team_name: envelope.affiliations.team_name.clone(),
                member_key: envelope.affiliations.member_key,
                workflow_key: envelope.affiliations.workflow_key,
                native_workflow_id: envelope.affiliations.native_workflow_id.clone(),
                completeness: envelope.affiliations.completeness,
                derived_from_revision_refs: envelope
                    .affiliations
                    .derived_from_revision_refs
                    .clone(),
            },
            source: ContinuitySourceWire {
                instance_key: envelope.source.instance_key,
                stream_key: envelope.source.stream_key,
                object_key: envelope.source.object_key,
                locator_id: None,
                generation: envelope.source.generation,
                source_record_id: None,
                record_index: None,
                cursor_start: None,
                cursor_end: None,
                byte_range: None,
            },
            native_time: None,
            observed_at: envelope.observed_at,
            phase: envelope.phase.into(),
            evidence: ContinuityEvidenceWire {
                authority: ContinuityEvidenceAuthorityWire::EngineControl,
                quality: QualifiedValueQuality::Derived,
                effective_at: None,
                completeness: ContractCompleteness::Complete,
            },
            event,
            native_evidence: ContinuityNativeEvidenceWire {
                kind: "engine_control".to_owned(),
            },
        };
        if &value.contract_selection != expected_selection {
            return Err(Self::invalid(
                "continuity envelope does not match the caller-held selection",
            ));
        }
        ObservationContractSelection::from_wire_value_for_expected(
            serde_json::to_value(&value.contract_selection)
                .expect("validated observation selection always serializes"),
            expected_selection,
        )
        .map_err(|error| Self::invalid(error.to_string()))?;
        value.validate_common(expected_root, expected_state)?;
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        expected_selection: &ObservationContractSelection,
        expected_root: &ScopedObservationRootIdentity,
        expected_state: &ScopedContinuityConsumerContext,
    ) -> Result<Self, ScopedContinuityEnvelopeContractError> {
        validate_raw_shape(&value, expected_root)?;
        let input: ScopedContinuityEnvelopeInput =
            serde_json::from_value(value).map_err(|error| Self::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            expected_selection,
        )
        .map_err(|error| Self::invalid(error.to_string()))?;
        let value = Self {
            scoped_continuity_envelope_contract_version: input
                .scoped_continuity_envelope_contract_version,
            contract_version: input.contract_version,
            contract_selection,
            observer_sequence: input.observer_sequence,
            scope_epoch: input.scope_epoch,
            event_id: input.event_id,
            semantic_revision_ref: input.semantic_revision_ref,
            root: input.root,
            actor: input.actor,
            actor_attribution: input.actor_attribution,
            affiliations: input.affiliations,
            source: input.source,
            native_time: input.native_time,
            observed_at: input.observed_at,
            phase: input.phase,
            evidence: input.evidence,
            event: input.event,
            native_evidence: input.native_evidence,
        };
        value.validate_common(expected_root, expected_state)?;
        Ok(value)
    }

    fn invalid(message: impl Into<String>) -> ScopedContinuityEnvelopeContractError {
        ScopedContinuityEnvelopeContractError::invalid(message)
    }

    fn validate_common(
        &self,
        expected_root: &ScopedObservationRootIdentity,
        expected_state: &ScopedContinuityConsumerContext,
    ) -> Result<(), ScopedContinuityEnvelopeContractError> {
        if self.scoped_continuity_envelope_contract_version
            != SCOPED_CONTINUITY_ENVELOPE_CONTRACT_VERSION
            || self.contract_version != self.contract_selection.envelope_contract_version
            || self.contract_selection.event_contract_version
                != SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
        {
            return Err(Self::invalid(
                "continuity envelope does not match the selected v1 contract",
            ));
        }
        require_positive_safe(self.observer_sequence, "observer_sequence")?;
        require_positive_safe(self.scope_epoch, "scope_epoch")?;
        require_safe_i64(self.observed_at, "observed_at")?;
        require_positive_safe(
            expected_state.current_scope_epoch,
            "caller current_scope_epoch",
        )?;
        require_safe_u64(
            expected_state.last_contiguous_sequence,
            "caller last_contiguous_sequence",
        )?;
        validate_context_baseline(expected_state)?;
        if let Some(prior) = expected_state.prior_resync_required.as_ref() {
            validate_prior_required(prior, expected_root, expected_state)?;
            if expected_state.last_contiguous_sequence != prior.control_sequence {
                return Err(Self::invalid(
                    "caller-held resync invalidation has not reached the contiguous boundary",
                ));
            }
        }
        if self.semantic_revision_ref.is_some()
            || self.root != ContinuityRootWire::from_root(expected_root)
            || self.actor != ContinuityActorWire::from_root(expected_root)
            || self.actor_attribution.kind != "scope_fallback"
            || self.actor_attribution.reason != "observer_lifecycle_control"
            || self.affiliations != ContinuityAffiliationsWire::from_root(expected_root)
            || self.native_time.is_some()
            || self.evidence.authority != ContinuityEvidenceAuthorityWire::EngineControl
            || self.evidence.quality != QualifiedValueQuality::Derived
            || self.evidence.effective_at.is_some()
            || self.evidence.completeness != ContractCompleteness::Complete
            || self.native_evidence.kind != "engine_control"
            || self.source != ContinuitySourceWire::from_root(expected_root, self.scope_epoch)?
        {
            return Err(Self::invalid(
                "continuity envelope does not match caller-held root or engine-control evidence",
            ));
        }

        let expected_event_id = match &self.event {
            ContinuityControlEventWire::ResyncRequired {
                invalid_scope_epoch,
                control_sequence,
                last_contiguous_sequence,
                baseline_snapshot_digest,
                reason,
                discarded_semantic_events,
                discarded_source_controls,
                discarded_retained_native_bytes,
            } => {
                require_positive_safe(*invalid_scope_epoch, "invalid_scope_epoch")?;
                require_positive_safe(*control_sequence, "control_sequence")?;
                require_safe_u64(*last_contiguous_sequence, "last_contiguous_sequence")?;
                require_safe_u64(*discarded_semantic_events, "discarded_semantic_events")?;
                require_safe_u64(*discarded_source_controls, "discarded_source_controls")?;
                require_safe_u64(
                    *discarded_retained_native_bytes,
                    "discarded_retained_native_bytes",
                )?;
                let baseline = decode_digest(baseline_snapshot_digest, "baseline_snapshot_digest")?;
                if self.phase != ContinuityDeliveryPhaseWire::Live
                    || expected_state.phase != ScopedAppendDeliveryPhase::Live
                    || self.scope_epoch != *invalid_scope_epoch
                    || self.observer_sequence != *control_sequence
                    || *control_sequence <= *last_contiguous_sequence
                    || *invalid_scope_epoch != expected_state.current_scope_epoch
                    || *last_contiguous_sequence != expected_state.last_contiguous_sequence
                    || expected_state.baseline_snapshot_digest != Some(baseline)
                    || expected_state.prior_resync_required.is_some()
                {
                    return Err(Self::invalid(
                        "resync-required control does not match caller-held continuity state",
                    ));
                }
                resync_required_event_id(
                    expected_root,
                    *invalid_scope_epoch,
                    (*reason).into(),
                    baseline,
                )
            }
            ContinuityControlEventWire::ResyncStarted {
                old_scope_epoch,
                new_scope_epoch,
                control_sequence,
                required_control_sequence,
                baseline_snapshot_digest,
                reason,
                replacement,
            } => {
                require_positive_safe(*old_scope_epoch, "old_scope_epoch")?;
                require_positive_safe(*new_scope_epoch, "new_scope_epoch")?;
                require_positive_safe(*control_sequence, "control_sequence")?;
                require_positive_safe(*required_control_sequence, "required_control_sequence")?;
                let baseline = decode_digest(baseline_snapshot_digest, "baseline_snapshot_digest")?;
                let prior = expected_state
                    .prior_resync_required
                    .as_ref()
                    .ok_or_else(|| {
                        Self::invalid(
                            "resync-started control requires caller-held delivered invalidation",
                        )
                    })?;
                if self.phase != ContinuityDeliveryPhaseWire::Correction
                    || expected_state.phase != ScopedAppendDeliveryPhase::Live
                    || self.scope_epoch != *new_scope_epoch
                    || self.observer_sequence != *control_sequence
                    || old_scope_epoch.checked_add(1) != Some(*new_scope_epoch)
                    || *old_scope_epoch != expected_state.current_scope_epoch
                    || expected_state.last_contiguous_sequence != prior.control_sequence
                    || *required_control_sequence != prior.control_sequence
                    || *control_sequence <= *required_control_sequence
                    || expected_state.baseline_snapshot_digest != Some(baseline)
                    || baseline != prior.baseline_snapshot_digest
                    || ScopedResyncReason::from(*reason) != prior.reason
                    || *replacement != ReplacementModeWire::FullSnapshot
                {
                    return Err(Self::invalid(
                        "resync-started control does not continue the caller-held invalidation",
                    ));
                }
                let control = ScopedResyncStarted {
                    root: expected_root.clone(),
                    old_scope_epoch: *old_scope_epoch,
                    new_scope_epoch: *new_scope_epoch,
                    control_sequence: *control_sequence,
                    required_control_sequence: *required_control_sequence,
                    baseline_snapshot_digest: baseline,
                    reason: (*reason).into(),
                    replacement: ScopedReplacementMode::FullSnapshot,
                };
                resync_started_event_id(expected_root, &control)
            }
            ContinuityControlEventWire::Failed {
                failed_scope_epoch,
                control_sequence,
                last_contiguous_sequence,
                phase,
                reason,
                discarded_semantic_events,
                discarded_source_controls,
                discarded_retained_native_bytes,
            } => {
                require_positive_safe(*failed_scope_epoch, "failed_scope_epoch")?;
                require_positive_safe(*control_sequence, "control_sequence")?;
                require_safe_u64(*last_contiguous_sequence, "last_contiguous_sequence")?;
                require_safe_u64(*discarded_semantic_events, "discarded_semantic_events")?;
                require_safe_u64(*discarded_source_controls, "discarded_source_controls")?;
                require_safe_u64(
                    *discarded_retained_native_bytes,
                    "discarded_retained_native_bytes",
                )?;
                if self.phase != *phase
                    || self.phase != expected_state.phase.into()
                    || self.scope_epoch != *failed_scope_epoch
                    || self.observer_sequence != *control_sequence
                    || *control_sequence <= *last_contiguous_sequence
                    || *failed_scope_epoch != expected_state.current_scope_epoch
                    || *last_contiguous_sequence != expected_state.last_contiguous_sequence
                {
                    return Err(Self::invalid(
                        "observer-failed control does not match caller-held continuity state",
                    ));
                }
                observer_failed_event_id(expected_root, *failed_scope_epoch, (*reason).into())
            }
        };
        if decode_opaque_exact(&self.event_id, "event_id", DIGEST_BYTES)?
            != expected_event_id.as_bytes()
        {
            return Err(Self::invalid(
                "event_id does not match the exact observer lifecycle control",
            ));
        }
        Ok(())
    }
}

fn validate_context_baseline(
    state: &ScopedContinuityConsumerContext,
) -> Result<(), ScopedContinuityEnvelopeContractError> {
    let valid = matches!(
        (state.phase, state.baseline_snapshot_digest),
        (ScopedAppendDeliveryPhase::Bootstrap, None)
            | (
                ScopedAppendDeliveryPhase::Live | ScopedAppendDeliveryPhase::Correction,
                Some(_)
            )
    );
    if !valid {
        return Err(ScopedContinuityEnvelopeContractError::invalid(
            "caller-held baseline presence does not match its delivery phase",
        ));
    }
    if state.prior_resync_required.is_some() && state.phase != ScopedAppendDeliveryPhase::Live {
        return Err(ScopedContinuityEnvelopeContractError::invalid(
            "caller-held invalidation requires live baseline state",
        ));
    }
    Ok(())
}

fn validate_prior_required(
    prior: &ScopedResyncRequired,
    expected_root: &ScopedObservationRootIdentity,
    expected_state: &ScopedContinuityConsumerContext,
) -> Result<(), ScopedContinuityEnvelopeContractError> {
    require_positive_safe(prior.invalid_scope_epoch, "prior invalid_scope_epoch")?;
    require_positive_safe(prior.control_sequence, "prior control_sequence")?;
    require_safe_u64(
        prior.last_contiguous_sequence,
        "prior last_contiguous_sequence",
    )?;
    require_safe_u64(
        prior.discarded_semantic_events,
        "prior discarded_semantic_events",
    )?;
    require_safe_u64(
        prior.discarded_source_controls,
        "prior discarded_source_controls",
    )?;
    require_safe_u64(
        prior.discarded_retained_native_bytes,
        "prior discarded_retained_native_bytes",
    )?;
    if prior.root != *expected_root
        || prior.invalid_scope_epoch != expected_state.current_scope_epoch
        || prior.control_sequence <= prior.last_contiguous_sequence
        || expected_state.baseline_snapshot_digest != Some(prior.baseline_snapshot_digest)
    {
        return Err(ScopedContinuityEnvelopeContractError::invalid(
            "caller-held resync invalidation is inconsistent",
        ));
    }
    Ok(())
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    required: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedContinuityEnvelopeContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedContinuityEnvelopeContractError::invalid(format!("{label} must be an object"))
    })?;
    for field in required {
        if !object.contains_key(*field) {
            return Err(ScopedContinuityEnvelopeContractError::invalid(format!(
                "{label} is missing field {field}"
            )));
        }
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()))
    {
        return Err(ScopedContinuityEnvelopeContractError::invalid(format!(
            "{label} contains unknown field {field}"
        )));
    }
    Ok(object)
}

fn validate_raw_shape(
    value: &JsonValue,
    expected_root: &ScopedObservationRootIdentity,
) -> Result<(), ScopedContinuityEnvelopeContractError> {
    let envelope = exact_object(
        value,
        "scoped continuity envelope",
        &[
            "scoped_continuity_envelope_contract_version",
            "contract_version",
            "contract_selection",
            "observer_sequence",
            "scope_epoch",
            "event_id",
            "semantic_revision_ref",
            "root",
            "actor",
            "actor_attribution",
            "affiliations",
            "source",
            "native_time",
            "observed_at",
            "phase",
            "evidence",
            "event",
            "native_evidence",
        ],
    )?;
    let expected_root_value = serde_json::to_value(ContinuityRootWire::from_root(expected_root))
        .expect("validated continuity root always serializes");
    if envelope["root"] != expected_root_value {
        return Err(ScopedContinuityEnvelopeContractError::invalid(
            "continuity envelope does not exactly match the caller-held root",
        ));
    }
    let expected_actor = serde_json::to_value(ContinuityActorWire::from_root(expected_root))
        .expect("validated continuity actor always serializes");
    if envelope["actor"] != expected_actor {
        return Err(ScopedContinuityEnvelopeContractError::invalid(
            "continuity envelope actor is not the caller-held root actor",
        ));
    }
    let expected_affiliations =
        serde_json::to_value(ContinuityAffiliationsWire::from_root(expected_root))
            .expect("validated continuity affiliations always serialize");
    if envelope["affiliations"] != expected_affiliations {
        return Err(ScopedContinuityEnvelopeContractError::invalid(
            "continuity affiliations are not explicitly unknown",
        ));
    }
    exact_object(
        &envelope["actor_attribution"],
        "continuity actor attribution",
        &["kind", "reason"],
    )?;
    exact_object(
        &envelope["source"],
        "continuity source",
        &[
            "instance_key",
            "stream_key",
            "object_key",
            "locator_id",
            "generation",
            "source_record_id",
            "record_index",
            "cursor_start",
            "cursor_end",
            "byte_range",
        ],
    )?;
    exact_object(
        &envelope["evidence"],
        "continuity evidence",
        &["authority", "quality", "effective_at", "completeness"],
    )?;
    exact_object(
        &envelope["native_evidence"],
        "continuity native evidence",
        &["kind"],
    )?;
    Ok(())
}

fn encode_opaque(bytes: &[u8]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
    exact_len: usize,
) -> Result<Vec<u8>, ScopedContinuityEnvelopeContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedContinuityEnvelopeContractError::invalid(format!(
            "{label} is not a v1 opaque reference"
        ))
    })?;
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedContinuityEnvelopeContractError::invalid(format!(
            "{label} is not canonical base64url"
        ))
    })?;
    if decoded.len() != exact_len || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err(ScopedContinuityEnvelopeContractError::invalid(format!(
            "{label} has invalid length or encoding"
        )));
    }
    Ok(decoded)
}

fn decode_digest(
    value: &str,
    label: &str,
) -> Result<ScopedReplacementSnapshotDigest, ScopedContinuityEnvelopeContractError> {
    let decoded = decode_opaque_exact(value, label, DIGEST_BYTES)?;
    let mut bytes = [0; DIGEST_BYTES];
    bytes.copy_from_slice(&decoded);
    Ok(ScopedReplacementSnapshotDigest(bytes))
}

fn require_positive_safe(
    value: u64,
    label: &str,
) -> Result<(), ScopedContinuityEnvelopeContractError> {
    if value == 0 {
        return Err(ScopedContinuityEnvelopeContractError::invalid(format!(
            "{label} must be positive"
        )));
    }
    require_safe_u64(value, label)
}

fn require_safe_u64(value: u64, label: &str) -> Result<(), ScopedContinuityEnvelopeContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedContinuityEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable integer range"
        )));
    }
    Ok(())
}

fn require_safe_i64(value: i64, label: &str) -> Result<(), ScopedContinuityEnvelopeContractError> {
    if value.unsigned_abs() > JS_SAFE_INTEGER_MAX {
        return Err(ScopedContinuityEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable integer range"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
