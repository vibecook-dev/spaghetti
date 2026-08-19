//! Strict contextual RFC 012D bootstrap/resync-completion envelope.
//!
//! This composes the already-frozen capability, replacement-manifest,
//! scope-coverage, artifact-availability, source-coverage, and ordered-envelope
//! contracts. The caller-held context is minted only by the authorized
//! attachment drain. Received values have no unbound `Deserialize` path and
//! cannot establish their own root, support release, source set, barrier
//! identity, queue boundary, or completion digests.

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::adapter::{
    ActorRunRole, ContractCompleteness, CoverageError, QualifiedValueQuality, SourceCoverageSet,
};
use crate::observation_contract::ObservationContractSelection;

use super::artifact_availability_wire::{
    ScopedArtifactAvailabilityConsumerContext, ScopedArtifactAvailabilityContextWire,
    ScopedArtifactAvailabilitySnapshotWire,
};
use super::capability_snapshot_wire::{
    ScopedCapabilitySnapshotConsumerContext, ScopedCapabilitySnapshotContextWire,
    ScopedCapabilitySnapshotWire,
};
use super::replacement_manifest_wire::{
    ScopedReplacementManifestConsumerContext, ScopedReplacementManifestContextWire,
    ScopedReplacementManifestWire,
};
use super::scope_coverage_wire::{
    ScopedScopeCoverageConsumerContext, ScopedScopeCoverageContextWire, ScopedScopeCoverageWire,
};
use super::source_wire::{ScopedSourceEnvelopeWire, SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION};
use super::{
    bootstrap_barrier_snapshot_is_valid, bootstrap_complete_event_id, observer_control_source,
    resync_barrier_snapshot_is_valid, resync_complete_event_id, source_presence_event_id,
    ScopedActorAttribution, ScopedActorFallbackReason, ScopedAppendDeliveryPhase,
    ScopedAppendPresenceChange, ScopedBootstrapBarrier, ScopedBootstrapSnapshotDigest,
    ScopedCompletionSnapshotComponents, ScopedEnvelopeEvidenceAuthority, ScopedNativeEvidence,
    ScopedObservationContinuity, ScopedObservationDeliveryState, ScopedObservationEnvelope,
    ScopedObservationEvent, ScopedObservationEventId, ScopedObservationRootIdentity,
    ScopedReplacementMode, ScopedReplacementSnapshotDigest, ScopedResyncBarrier,
    ScopedSourceObjectIdentity, SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION,
    SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION, SCOPED_RESYNC_BARRIER_CONTRACT_VERSION,
};

pub(crate) const SCOPED_COMPLETION_ENVELOPE_CONTRACT_VERSION: u32 = 1;

const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const DIGEST_ENCODED_BYTES: usize = 43;
const MAX_SOURCE_COVERAGE_SETS: usize = 64;
const MAX_COVERAGE_ERRORS_PER_SET: usize = 4_096;
const MAX_EXPLICIT_OBJECT_ERRORS: usize = MAX_SOURCE_COVERAGE_SETS * MAX_COVERAGE_ERRORS_PER_SET;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedCompletionEnvelopeContractError {
    #[error("invalid scoped completion envelope contract: {message}")]
    Invalid { message: String },
    #[error("scoped envelope event is not supported by the completion wire contract")]
    UnsupportedEvent,
    #[error("scoped completion envelope does not match caller-held context")]
    ContextMismatch,
}

impl ScopedCompletionEnvelopeContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }

    fn nested(error: impl std::fmt::Display) -> Self {
        Self::invalid(error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompletionDeliveryPhaseWire {
    Bootstrap,
    Correction,
}

impl CompletionDeliveryPhaseWire {
    fn from_internal(
        value: ScopedAppendDeliveryPhase,
    ) -> Result<Self, ScopedCompletionEnvelopeContractError> {
        match value {
            ScopedAppendDeliveryPhase::Bootstrap => Ok(Self::Bootstrap),
            ScopedAppendDeliveryPhase::Correction => Ok(Self::Correction),
            ScopedAppendDeliveryPhase::Live => {
                Err(ScopedCompletionEnvelopeContractError::UnsupportedEvent)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompletionContinuityWire {
    Valid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionQueueStateWire {
    scope_epoch: u64,
    offered_through_sequence: u64,
    delivered_through_sequence: u64,
    continuity: CompletionContinuityWire,
    queued_semantic_events: u64,
    queued_retained_native_bytes: u64,
    queued_source_control_items: u64,
}

impl CompletionQueueStateWire {
    fn from_internal(
        value: ScopedObservationDeliveryState,
    ) -> Result<Self, ScopedCompletionEnvelopeContractError> {
        if value.continuity != ScopedObservationContinuity::Valid {
            return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
        }
        let result = Self {
            scope_epoch: value.scope_epoch,
            offered_through_sequence: value.offered_through_sequence,
            delivered_through_sequence: value.delivered_through_sequence,
            continuity: CompletionContinuityWire::Valid,
            queued_semantic_events: u64::try_from(value.queued_semantic_events)
                .map_err(|_| ScopedCompletionEnvelopeContractError::ContextMismatch)?,
            queued_retained_native_bytes: value.queued_retained_native_bytes,
            queued_source_control_items: u64::try_from(value.queued_source_control_items)
                .map_err(|_| ScopedCompletionEnvelopeContractError::ContextMismatch)?,
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), ScopedCompletionEnvelopeContractError> {
        for (label, value) in [
            ("queue scope_epoch", self.scope_epoch),
            (
                "queue offered_through_sequence",
                self.offered_through_sequence,
            ),
        ] {
            validate_positive_portable(label, value)?;
        }
        for (label, value) in [
            (
                "queue delivered_through_sequence",
                self.delivered_through_sequence,
            ),
            ("queue semantic event count", self.queued_semantic_events),
            (
                "queue retained-native byte count",
                self.queued_retained_native_bytes,
            ),
            (
                "queue source-control item count",
                self.queued_source_control_items,
            ),
        ] {
            validate_nonnegative_portable(label, value)?;
        }
        if self.delivered_through_sequence >= self.offered_through_sequence
            || self.queued_source_control_items == 0
        {
            return Err(ScopedCompletionEnvelopeContractError::invalid(
                "completion queue state does not retain the offered barrier",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct CompletionRootWire {
    session_ref: crate::adapter::ExternalEntityRef,
    session_key: crate::adapter::CanonicalEntityKey,
    root_actor_run_key: crate::adapter::CanonicalEntityKey,
    native_session_claim: Option<crate::adapter::NativeIdentityClaim>,
}

impl CompletionRootWire {
    fn from_root(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            session_ref: root.session_ref,
            session_key: root.session_key,
            root_actor_run_key: root.root_actor_run_key,
            native_session_claim: root.native_session_claim.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct CompletionSourceBindingWire {
    instance_key: crate::adapter::CanonicalSourceInstanceKey,
    stream_key: crate::adapter::CoverageStreamKey,
    object_key: crate::adapter::CoverageObjectKey,
}

impl CompletionSourceBindingWire {
    fn from_source(source: &ScopedSourceObjectIdentity) -> Self {
        Self {
            instance_key: source.source_instance_key,
            stream_key: source.stream_key,
            object_key: source.object_key,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedCompletionBarrier {
    Bootstrap {
        snapshot_digest: ScopedBootstrapSnapshotDigest,
    },
    Resync {
        started_control_sequence: u64,
        coverage_snapshot_digest: ScopedBootstrapSnapshotDigest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ExpectedCompletionBarrierWire {
    Bootstrap {
        snapshot_digest: String,
        replacement_snapshot_digest: String,
    },
    Resync {
        started_control_sequence: u64,
        coverage_snapshot_digest: String,
        replacement_snapshot_digest: String,
    },
}

/// Exact in-process authority held by the sole ordered consumer. It is
/// intentionally non-Serde and its redacted Debug omits support-release,
/// program, source, coverage, artifact, family, and digest details.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedCompletionEnvelopeConsumerContext {
    contract_selection: ObservationContractSelection,
    root: ScopedObservationRootIdentity,
    expected_source: ScopedSourceObjectIdentity,
    expected_observer_sequence: u64,
    expected_scope_epoch: u64,
    expected_event_id: ScopedObservationEventId,
    expected_observed_at: i64,
    expected_phase: CompletionDeliveryPhaseWire,
    expected_barrier: ExpectedCompletionBarrier,
    expected_replacement_snapshot_digest: ScopedReplacementSnapshotDigest,
    expected_queue_state: CompletionQueueStateWire,
    expected_root_present: bool,
    expected_source_coverage: Vec<SourceCoverageSet>,
    expected_explicit_object_errors: Vec<CoverageError>,
    capability_context: Arc<ScopedCapabilitySnapshotConsumerContext>,
    replacement_manifest_context: ScopedReplacementManifestConsumerContext,
    scope_coverage_context: ScopedScopeCoverageConsumerContext,
    artifact_availability_context: ScopedArtifactAvailabilityConsumerContext,
}

impl std::fmt::Debug for ScopedCompletionEnvelopeConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedCompletionEnvelopeConsumerContext")
            .field(
                "barrier_kind",
                &match self.expected_barrier {
                    ExpectedCompletionBarrier::Bootstrap { .. } => "bootstrap",
                    ExpectedCompletionBarrier::Resync { .. } => "resync",
                },
            )
            .field("observer_sequence", &self.expected_observer_sequence)
            .field("scope_epoch", &self.expected_scope_epoch)
            .field(
                "source_coverage_set_count",
                &self.expected_source_coverage.len(),
            )
            .field(
                "explicit_object_error_count",
                &self.expected_explicit_object_errors.len(),
            )
            .finish_non_exhaustive()
    }
}

impl ScopedCompletionEnvelopeConsumerContext {
    pub(crate) fn from_scoped_envelope(
        envelope: &ScopedObservationEnvelope,
        expected_root: &ScopedObservationRootIdentity,
        capability_context: Arc<ScopedCapabilitySnapshotConsumerContext>,
    ) -> Result<Option<Self>, ScopedCompletionEnvelopeContractError> {
        let expected_source = observer_control_source(expected_root)
            .map_err(|()| ScopedCompletionEnvelopeContractError::ContextMismatch)?;
        let (
            expected_barrier,
            expected_replacement_snapshot_digest,
            expected_queue_state,
            expected_root_present,
            expected_source_coverage,
            expected_explicit_object_errors,
            capability_snapshot,
            replacement_manifest_context,
            scope_coverage_context,
            artifact_availability_context,
        ) = match &envelope.event {
            ScopedObservationEvent::ObserverBootstrapComplete { barrier } => {
                validate_bootstrap_barrier(barrier, expected_root, envelope)?;
                let contexts = component_contexts(
                    &envelope.contract_selection,
                    &capability_context,
                    ScopedCompletionSnapshotComponents {
                        root: expected_root,
                        root_present: barrier.root_present,
                        family_manifest: &barrier.family_manifest,
                        observation_capabilities: &barrier.observation_capabilities,
                        scope_coverage: &barrier.scope_coverage,
                        source_coverage: &barrier.source_coverage,
                        explicit_object_errors: &barrier.explicit_object_errors,
                        artifact_availability: &barrier.artifact_availability,
                    },
                )?;
                (
                    ExpectedCompletionBarrier::Bootstrap {
                        snapshot_digest: barrier.snapshot_digest,
                    },
                    barrier.replacement_snapshot_digest,
                    CompletionQueueStateWire::from_internal(barrier.queue_state)?,
                    barrier.root_present,
                    barrier.source_coverage.clone(),
                    barrier.explicit_object_errors.clone(),
                    contexts.0,
                    contexts.1,
                    contexts.2,
                    contexts.3,
                )
            }
            ScopedObservationEvent::ObserverResyncComplete { barrier } => {
                validate_resync_barrier(barrier, expected_root, envelope)?;
                let contexts = component_contexts(
                    &envelope.contract_selection,
                    &capability_context,
                    ScopedCompletionSnapshotComponents {
                        root: expected_root,
                        root_present: barrier.root_present,
                        family_manifest: &barrier.family_manifest,
                        observation_capabilities: &barrier.observation_capabilities,
                        scope_coverage: &barrier.scope_coverage,
                        source_coverage: &barrier.source_coverage,
                        explicit_object_errors: &barrier.explicit_object_errors,
                        artifact_availability: &barrier.artifact_availability,
                    },
                )?;
                (
                    ExpectedCompletionBarrier::Resync {
                        started_control_sequence: barrier.started_control_sequence,
                        coverage_snapshot_digest: barrier.coverage_snapshot_digest,
                    },
                    barrier.replacement_snapshot_digest,
                    CompletionQueueStateWire::from_internal(barrier.queue_state)?,
                    barrier.root_present,
                    barrier.source_coverage.clone(),
                    barrier.explicit_object_errors.clone(),
                    contexts.0,
                    contexts.1,
                    contexts.2,
                    contexts.3,
                )
            }
            _ => return Ok(None),
        };
        let canonical_errors = canonical_explicit_errors(&expected_source_coverage)?;
        if expected_explicit_object_errors != canonical_errors
            || expected_source_coverage.is_empty()
            || expected_source_coverage.len() > MAX_SOURCE_COVERAGE_SETS
            || expected_explicit_object_errors.len() > MAX_EXPLICIT_OBJECT_ERRORS
            || !source_coverage_matches_authority(
                expected_root,
                &capability_context,
                &expected_source_coverage,
            )
        {
            return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
        }
        let expected_phase = CompletionDeliveryPhaseWire::from_internal(envelope.phase)?;
        let context = Self {
            contract_selection: envelope.contract_selection.clone(),
            root: expected_root.clone(),
            expected_source,
            expected_observer_sequence: envelope.observer_sequence,
            expected_scope_epoch: envelope.scope_epoch,
            expected_event_id: envelope.event_id,
            expected_observed_at: envelope.observed_at,
            expected_phase,
            expected_barrier,
            expected_replacement_snapshot_digest,
            expected_queue_state,
            expected_root_present,
            expected_source_coverage,
            expected_explicit_object_errors,
            capability_context,
            replacement_manifest_context,
            scope_coverage_context,
            artifact_availability_context,
        };
        if capability_snapshot
            != ScopedCapabilitySnapshotWire::from_context(&context.capability_context)
                .map_err(ScopedCompletionEnvelopeContractError::nested)?
        {
            return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
        }
        ScopedCompletionEnvelopeWire::from_scoped_for_context(envelope, &context)?;
        Ok(Some(context))
    }

    pub(crate) fn wire(&self) -> ScopedCompletionEnvelopeContextWire {
        let replacement_snapshot_digest =
            encode_opaque(self.expected_replacement_snapshot_digest.as_bytes());
        let expected_barrier = match self.expected_barrier {
            ExpectedCompletionBarrier::Bootstrap { snapshot_digest } => {
                ExpectedCompletionBarrierWire::Bootstrap {
                    snapshot_digest: encode_opaque(snapshot_digest.as_bytes()),
                    replacement_snapshot_digest,
                }
            }
            ExpectedCompletionBarrier::Resync {
                started_control_sequence,
                coverage_snapshot_digest,
            } => ExpectedCompletionBarrierWire::Resync {
                started_control_sequence,
                coverage_snapshot_digest: encode_opaque(coverage_snapshot_digest.as_bytes()),
                replacement_snapshot_digest,
            },
        };
        ScopedCompletionEnvelopeContextWire {
            contract_selection: self.contract_selection.clone(),
            adapter_id: self.root.adapter_id.as_str().to_owned(),
            root: CompletionRootWire::from_root(&self.root),
            expected_source: CompletionSourceBindingWire::from_source(&self.expected_source),
            expected_observer_sequence: self.expected_observer_sequence,
            expected_scope_epoch: self.expected_scope_epoch,
            expected_event_id: encode_opaque(self.expected_event_id.as_bytes()),
            expected_observed_at: self.expected_observed_at,
            expected_phase: self.expected_phase,
            expected_barrier,
            expected_queue_state: self.expected_queue_state,
            expected_root_present: self.expected_root_present,
            capability_context: self.capability_context.wire(),
            replacement_manifest_context: self.replacement_manifest_context.wire(),
            scope_coverage_context: self.scope_coverage_context.wire(),
            artifact_availability_context: self.artifact_availability_context.wire(),
        }
    }
}

fn source_coverage_matches_authority(
    root: &ScopedObservationRootIdentity,
    capability_context: &ScopedCapabilitySnapshotConsumerContext,
    source_coverage: &[SourceCoverageSet],
) -> bool {
    source_coverage.iter().all(|set| {
        set.scope.adapter_id == root.adapter_id.as_str()
            && set.scope.source_instance_key == root.source_instance_key
            && set.scope.root_entity_key == Some(root.session_key)
            && set.scope.support_release_id == capability_context.support_release_id()
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedCompletionEnvelopeContextWire {
    contract_selection: ObservationContractSelection,
    adapter_id: String,
    root: CompletionRootWire,
    expected_source: CompletionSourceBindingWire,
    expected_observer_sequence: u64,
    expected_scope_epoch: u64,
    expected_event_id: String,
    expected_observed_at: i64,
    expected_phase: CompletionDeliveryPhaseWire,
    expected_barrier: ExpectedCompletionBarrierWire,
    expected_queue_state: CompletionQueueStateWire,
    expected_root_present: bool,
    capability_context: ScopedCapabilitySnapshotContextWire,
    replacement_manifest_context: ScopedReplacementManifestContextWire,
    scope_coverage_context: ScopedScopeCoverageContextWire,
    artifact_availability_context: ScopedArtifactAvailabilityContextWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct BootstrapBarrierWire {
    barrier_contract_version: u32,
    scope_epoch: u64,
    barrier_sequence: u64,
    snapshot_digest: String,
    replacement_snapshot_digest: String,
    replacement_manifest: ScopedReplacementManifestWire,
    capability_snapshot: ScopedCapabilitySnapshotWire,
    source_coverage: Vec<SourceCoverageSet>,
    scope_coverage: ScopedScopeCoverageWire,
    explicit_object_errors: Vec<CoverageError>,
    artifact_availability: ScopedArtifactAvailabilitySnapshotWire,
    queue_state: CompletionQueueStateWire,
    root_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ResyncBarrierWire {
    barrier_contract_version: u32,
    scope_epoch: u64,
    replacement: String,
    started_control_sequence: u64,
    barrier_sequence: u64,
    replacement_snapshot_digest: String,
    coverage_snapshot_digest: String,
    replacement_manifest: ScopedReplacementManifestWire,
    capability_snapshot: ScopedCapabilitySnapshotWire,
    source_coverage: Vec<SourceCoverageSet>,
    scope_coverage: ScopedScopeCoverageWire,
    explicit_object_errors: Vec<CoverageError>,
    artifact_availability: ScopedArtifactAvailabilitySnapshotWire,
    queue_state: CompletionQueueStateWire,
    root_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CompletionEventWire {
    ObserverBootstrapComplete { barrier: BootstrapBarrierWire },
    ObserverResyncComplete { barrier: ResyncBarrierWire },
}

/// Serialize-only completion envelope. Received JSON must be consumed through
/// `from_wire_value_for_context` with the original in-process context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedCompletionEnvelopeWire {
    scoped_completion_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: ObservationContractSelection,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: JsonValue,
    root: JsonValue,
    actor: JsonValue,
    actor_attribution: JsonValue,
    affiliations: JsonValue,
    source: JsonValue,
    native_time: JsonValue,
    observed_at: i64,
    phase: CompletionDeliveryPhaseWire,
    evidence: JsonValue,
    event: CompletionEventWire,
    native_evidence: JsonValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedCompletionEnvelopeInput {
    scoped_completion_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: JsonValue,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: JsonValue,
    root: JsonValue,
    actor: JsonValue,
    actor_attribution: JsonValue,
    affiliations: JsonValue,
    source: JsonValue,
    native_time: JsonValue,
    observed_at: i64,
    phase: CompletionDeliveryPhaseWire,
    evidence: JsonValue,
    event: JsonValue,
    native_evidence: JsonValue,
}

impl ScopedCompletionEnvelopeWire {
    pub(crate) fn from_scoped_for_context(
        envelope: &ScopedObservationEnvelope,
        context: &ScopedCompletionEnvelopeConsumerContext,
    ) -> Result<Self, ScopedCompletionEnvelopeContractError> {
        let event = event_from_scoped(envelope, context)?;
        let value = Self {
            scoped_completion_envelope_contract_version:
                SCOPED_COMPLETION_ENVELOPE_CONTRACT_VERSION,
            contract_version: envelope.contract_version,
            contract_selection: envelope.contract_selection.clone(),
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
            event_id: encode_opaque(envelope.event_id.as_bytes()),
            semantic_revision_ref: JsonValue::Null,
            root: envelope_root_value(envelope),
            actor: envelope_actor_value(envelope),
            actor_attribution: observer_attribution_value(envelope)?,
            affiliations: envelope_affiliations_value(envelope),
            source: envelope_source_value(envelope)?,
            native_time: JsonValue::Null,
            observed_at: envelope.observed_at,
            phase: CompletionDeliveryPhaseWire::from_internal(envelope.phase)?,
            evidence: completion_evidence_value(envelope)?,
            event,
            native_evidence: completion_native_evidence_value(envelope)?,
        };
        value.validate_against(context)?;
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &ScopedCompletionEnvelopeConsumerContext,
    ) -> Result<Self, ScopedCompletionEnvelopeContractError> {
        preflight_wire_value(&value, context)?;
        let input: ScopedCompletionEnvelopeInput =
            serde_json::from_value(value).map_err(ScopedCompletionEnvelopeContractError::nested)?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            &context.contract_selection,
        )
        .map_err(ScopedCompletionEnvelopeContractError::nested)?;
        let event = parse_event_value(input.event, context)?;
        let value = Self {
            scoped_completion_envelope_contract_version: input
                .scoped_completion_envelope_contract_version,
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
            event,
            native_evidence: input.native_evidence,
        };
        value.validate_against(context)?;
        Ok(value)
    }

    fn validate_against(
        &self,
        context: &ScopedCompletionEnvelopeConsumerContext,
    ) -> Result<(), ScopedCompletionEnvelopeContractError> {
        validate_positive_portable("observer_sequence", self.observer_sequence)?;
        validate_positive_portable("scope_epoch", self.scope_epoch)?;
        validate_portable_i64("observed_at", self.observed_at)?;
        let event_id = decode_opaque_exact(&self.event_id, "completion event_id")?;
        if self.scoped_completion_envelope_contract_version
            != SCOPED_COMPLETION_ENVELOPE_CONTRACT_VERSION
            || self.contract_version != self.contract_selection.envelope_contract_version
            || self.contract_selection != context.contract_selection
            || self.contract_selection.event_contract_version
                != SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
            || self.observer_sequence != context.expected_observer_sequence
            || self.scope_epoch != context.expected_scope_epoch
            || event_id != *context.expected_event_id.as_bytes()
            || self.observed_at != context.expected_observed_at
            || self.phase != context.expected_phase
            || !self.semantic_revision_ref.is_null()
            || !self.native_time.is_null()
            || self.root != root_wire_value(&context.root)
            || self.source
                != source_wire_value(&context.expected_source, context.expected_scope_epoch)
            || self.actor_attribution
                != json!({
                    "kind": "scope_fallback",
                    "reason": "observer_lifecycle_control",
                })
            || self.evidence
                != json!({
                    "authority": "engine_control",
                    "quality": "derived",
                    "effective_at": null,
                    "completeness": "complete",
                })
            || self.native_evidence != json!({"kind": "engine_control"})
            || !event_matches_context(&self.event, context)
        {
            return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
        }
        self.validate_common_via_source_contract(context)
    }

    fn validate_common_via_source_contract(
        &self,
        context: &ScopedCompletionEnvelopeConsumerContext,
    ) -> Result<(), ScopedCompletionEnvelopeContractError> {
        let mut proxy =
            serde_json::to_value(self).map_err(ScopedCompletionEnvelopeContractError::nested)?;
        let object = proxy.as_object_mut().ok_or_else(|| {
            ScopedCompletionEnvelopeContractError::invalid("completion envelope must be an object")
        })?;
        object.remove("scoped_completion_envelope_contract_version");
        object.insert(
            "scoped_source_envelope_contract_version".to_owned(),
            json!(SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION),
        );
        object.insert(
            "event_id".to_owned(),
            json!(encode_opaque(
                source_presence_event_id(
                    &context.expected_source,
                    ScopedAppendPresenceChange::Created {
                        generation: context.expected_scope_epoch,
                    },
                )
                .as_bytes(),
            )),
        );
        object.insert(
            "actor_attribution".to_owned(),
            json!({
                "kind": "scope_fallback",
                "reason": "source_lifecycle_control",
            }),
        );
        object.insert(
            "event".to_owned(),
            json!({
                "kind": "source_created",
                "generation": context.expected_scope_epoch,
            }),
        );
        ScopedSourceEnvelopeWire::from_wire_value_for_context(
            proxy,
            &context.contract_selection,
            &context.root,
            std::slice::from_ref(&context.expected_source),
        )
        .map_err(ScopedCompletionEnvelopeContractError::nested)?;
        Ok(())
    }
}

fn component_contexts(
    selection: &ObservationContractSelection,
    capability_context: &ScopedCapabilitySnapshotConsumerContext,
    snapshot: ScopedCompletionSnapshotComponents<'_>,
) -> Result<
    (
        ScopedCapabilitySnapshotWire,
        ScopedReplacementManifestConsumerContext,
        ScopedScopeCoverageConsumerContext,
        ScopedArtifactAvailabilityConsumerContext,
    ),
    ScopedCompletionEnvelopeContractError,
> {
    let capability_snapshot =
        ScopedCapabilitySnapshotWire::from_capabilities(snapshot.observation_capabilities)
            .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    ScopedCapabilitySnapshotWire::from_wire_value_for_context(
        serde_json::to_value(&capability_snapshot)
            .map_err(ScopedCompletionEnvelopeContractError::nested)?,
        capability_context,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let replacement_manifest_context = ScopedReplacementManifestConsumerContext::from_expected(
        selection,
        snapshot.source_coverage,
        snapshot.family_manifest,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let scope_coverage_context = ScopedScopeCoverageConsumerContext::from_expected(
        snapshot.scope_coverage,
        snapshot.root,
        snapshot.source_coverage,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let artifact_availability_context = ScopedArtifactAvailabilityConsumerContext::from_expected(
        selection,
        snapshot.root.session_key,
        snapshot.artifact_availability,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    Ok((
        capability_snapshot,
        replacement_manifest_context,
        scope_coverage_context,
        artifact_availability_context,
    ))
}

fn validate_bootstrap_barrier(
    barrier: &ScopedBootstrapBarrier,
    root: &ScopedObservationRootIdentity,
    envelope: &ScopedObservationEnvelope,
) -> Result<(), ScopedCompletionEnvelopeContractError> {
    if barrier.barrier_contract_version != SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION
        || barrier.root != *root
        || barrier.scope_epoch != envelope.scope_epoch
        || barrier.barrier_sequence != envelope.observer_sequence
        || barrier.queue_state.scope_epoch != envelope.scope_epoch
        || barrier.queue_state.offered_through_sequence != envelope.observer_sequence
        || barrier.queue_state.continuity != ScopedObservationContinuity::Valid
        || bootstrap_complete_event_id(
            root,
            barrier.scope_epoch,
            barrier.replacement_snapshot_digest,
        ) != envelope.event_id
        || !bootstrap_barrier_snapshot_is_valid(barrier)
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(())
}

fn validate_resync_barrier(
    barrier: &ScopedResyncBarrier,
    root: &ScopedObservationRootIdentity,
    envelope: &ScopedObservationEnvelope,
) -> Result<(), ScopedCompletionEnvelopeContractError> {
    if barrier.barrier_contract_version != SCOPED_RESYNC_BARRIER_CONTRACT_VERSION
        || barrier.root != *root
        || barrier.scope_epoch != envelope.scope_epoch
        || barrier.replacement != ScopedReplacementMode::FullSnapshot
        || barrier.started_control_sequence >= barrier.barrier_sequence
        || barrier.barrier_sequence != envelope.observer_sequence
        || barrier.queue_state.scope_epoch != envelope.scope_epoch
        || barrier.queue_state.offered_through_sequence != envelope.observer_sequence
        || barrier.queue_state.continuity != ScopedObservationContinuity::Valid
        || resync_complete_event_id(root, barrier) != envelope.event_id
        || !resync_barrier_snapshot_is_valid(barrier)
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(())
}

fn event_from_scoped(
    envelope: &ScopedObservationEnvelope,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> Result<CompletionEventWire, ScopedCompletionEnvelopeContractError> {
    let event_value = match &envelope.event {
        ScopedObservationEvent::ObserverBootstrapComplete { barrier } => {
            let replacement_context = ScopedReplacementManifestConsumerContext::from_expected(
                &envelope.contract_selection,
                &barrier.source_coverage,
                &barrier.family_manifest,
            )
            .map_err(ScopedCompletionEnvelopeContractError::nested)?;
            let artifact_context = ScopedArtifactAvailabilityConsumerContext::from_expected(
                &envelope.contract_selection,
                context.root.session_key,
                &barrier.artifact_availability,
            )
            .map_err(ScopedCompletionEnvelopeContractError::nested)?;
            json!({
                "kind": "observer_bootstrap_complete",
                "barrier": {
                    "barrier_contract_version": barrier.barrier_contract_version,
                    "scope_epoch": barrier.scope_epoch,
                    "barrier_sequence": barrier.barrier_sequence,
                    "snapshot_digest": encode_opaque(barrier.snapshot_digest.as_bytes()),
                    "replacement_snapshot_digest": encode_opaque(barrier.replacement_snapshot_digest.as_bytes()),
                    "replacement_manifest": ScopedReplacementManifestWire::from_context(&replacement_context)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "capability_snapshot": ScopedCapabilitySnapshotWire::from_capabilities(&barrier.observation_capabilities)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "source_coverage": barrier.source_coverage,
                    "scope_coverage": ScopedScopeCoverageWire::from_expected(&barrier.scope_coverage, &context.scope_coverage_context)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "explicit_object_errors": barrier.explicit_object_errors,
                    "artifact_availability": ScopedArtifactAvailabilitySnapshotWire::from_context(&artifact_context)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "queue_state": CompletionQueueStateWire::from_internal(barrier.queue_state)?,
                    "root_present": barrier.root_present,
                }
            })
        }
        ScopedObservationEvent::ObserverResyncComplete { barrier } => {
            let replacement_context = ScopedReplacementManifestConsumerContext::from_expected(
                &envelope.contract_selection,
                &barrier.source_coverage,
                &barrier.family_manifest,
            )
            .map_err(ScopedCompletionEnvelopeContractError::nested)?;
            let artifact_context = ScopedArtifactAvailabilityConsumerContext::from_expected(
                &envelope.contract_selection,
                context.root.session_key,
                &barrier.artifact_availability,
            )
            .map_err(ScopedCompletionEnvelopeContractError::nested)?;
            json!({
                "kind": "observer_resync_complete",
                "barrier": {
                    "barrier_contract_version": barrier.barrier_contract_version,
                    "scope_epoch": barrier.scope_epoch,
                    "replacement": "full_snapshot",
                    "started_control_sequence": barrier.started_control_sequence,
                    "barrier_sequence": barrier.barrier_sequence,
                    "replacement_snapshot_digest": encode_opaque(barrier.replacement_snapshot_digest.as_bytes()),
                    "coverage_snapshot_digest": encode_opaque(barrier.coverage_snapshot_digest.as_bytes()),
                    "replacement_manifest": ScopedReplacementManifestWire::from_context(&replacement_context)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "capability_snapshot": ScopedCapabilitySnapshotWire::from_capabilities(&barrier.observation_capabilities)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "source_coverage": barrier.source_coverage,
                    "scope_coverage": ScopedScopeCoverageWire::from_expected(&barrier.scope_coverage, &context.scope_coverage_context)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "explicit_object_errors": barrier.explicit_object_errors,
                    "artifact_availability": ScopedArtifactAvailabilitySnapshotWire::from_context(&artifact_context)
                        .map_err(ScopedCompletionEnvelopeContractError::nested)?,
                    "queue_state": CompletionQueueStateWire::from_internal(barrier.queue_state)?,
                    "root_present": barrier.root_present,
                }
            })
        }
        _ => return Err(ScopedCompletionEnvelopeContractError::UnsupportedEvent),
    };
    parse_event_value(event_value, context)
}

fn parse_event_value(
    value: JsonValue,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> Result<CompletionEventWire, ScopedCompletionEnvelopeContractError> {
    let event = exact_object(&value, "completion event", &["kind", "barrier"])?;
    let kind = event
        .get("kind")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            ScopedCompletionEnvelopeContractError::invalid("completion event kind must be a string")
        })?;
    let barrier = event
        .get("barrier")
        .cloned()
        .expect("checked completion barrier exists");
    match (kind, context.expected_barrier) {
        ("observer_bootstrap_complete", ExpectedCompletionBarrier::Bootstrap { .. }) => {
            parse_bootstrap_barrier(barrier, context)
                .map(|barrier| CompletionEventWire::ObserverBootstrapComplete { barrier })
        }
        ("observer_resync_complete", ExpectedCompletionBarrier::Resync { .. }) => {
            parse_resync_barrier(barrier, context)
                .map(|barrier| CompletionEventWire::ObserverResyncComplete { barrier })
        }
        ("observer_bootstrap_complete" | "observer_resync_complete", _) => {
            Err(ScopedCompletionEnvelopeContractError::ContextMismatch)
        }
        _ => Err(ScopedCompletionEnvelopeContractError::UnsupportedEvent),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapBarrierInput {
    barrier_contract_version: u32,
    scope_epoch: u64,
    barrier_sequence: u64,
    snapshot_digest: String,
    replacement_snapshot_digest: String,
    replacement_manifest: JsonValue,
    capability_snapshot: JsonValue,
    source_coverage: JsonValue,
    scope_coverage: JsonValue,
    explicit_object_errors: JsonValue,
    artifact_availability: JsonValue,
    queue_state: CompletionQueueStateWire,
    root_present: bool,
}

fn parse_bootstrap_barrier(
    value: JsonValue,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> Result<BootstrapBarrierWire, ScopedCompletionEnvelopeContractError> {
    preflight_barrier_arrays(&value, context, false)?;
    let input: BootstrapBarrierInput =
        serde_json::from_value(value).map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let source_coverage = parse_source_coverage(input.source_coverage, context)?;
    let explicit_object_errors = parse_explicit_errors(input.explicit_object_errors, context)?;
    let replacement_manifest = ScopedReplacementManifestWire::from_wire_value_for_context(
        input.replacement_manifest,
        &context.replacement_manifest_context,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let capability_snapshot = ScopedCapabilitySnapshotWire::from_wire_value_for_context(
        input.capability_snapshot,
        &context.capability_context,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let scope_coverage = ScopedScopeCoverageWire::from_wire_value_for_context(
        input.scope_coverage,
        &context.scope_coverage_context,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let artifact_availability =
        ScopedArtifactAvailabilitySnapshotWire::from_wire_value_for_context(
            input.artifact_availability,
            &context.artifact_availability_context,
        )
        .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let ExpectedCompletionBarrier::Bootstrap { snapshot_digest } = context.expected_barrier else {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    };
    let snapshot = decode_opaque_exact(&input.snapshot_digest, "bootstrap snapshot digest")?;
    let replacement = decode_opaque_exact(
        &input.replacement_snapshot_digest,
        "bootstrap replacement snapshot digest",
    )?;
    input.queue_state.validate()?;
    if input.barrier_contract_version != SCOPED_BOOTSTRAP_BARRIER_CONTRACT_VERSION
        || input.scope_epoch != context.expected_scope_epoch
        || input.barrier_sequence != context.expected_observer_sequence
        || snapshot != *snapshot_digest.as_bytes()
        || replacement != *context.expected_replacement_snapshot_digest.as_bytes()
        || input.queue_state != context.expected_queue_state
        || input.root_present != context.expected_root_present
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(BootstrapBarrierWire {
        barrier_contract_version: input.barrier_contract_version,
        scope_epoch: input.scope_epoch,
        barrier_sequence: input.barrier_sequence,
        snapshot_digest: input.snapshot_digest,
        replacement_snapshot_digest: input.replacement_snapshot_digest,
        replacement_manifest,
        capability_snapshot,
        source_coverage,
        scope_coverage,
        explicit_object_errors,
        artifact_availability,
        queue_state: input.queue_state,
        root_present: input.root_present,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResyncBarrierInput {
    barrier_contract_version: u32,
    scope_epoch: u64,
    replacement: String,
    started_control_sequence: u64,
    barrier_sequence: u64,
    replacement_snapshot_digest: String,
    coverage_snapshot_digest: String,
    replacement_manifest: JsonValue,
    capability_snapshot: JsonValue,
    source_coverage: JsonValue,
    scope_coverage: JsonValue,
    explicit_object_errors: JsonValue,
    artifact_availability: JsonValue,
    queue_state: CompletionQueueStateWire,
    root_present: bool,
}

fn parse_resync_barrier(
    value: JsonValue,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> Result<ResyncBarrierWire, ScopedCompletionEnvelopeContractError> {
    preflight_barrier_arrays(&value, context, true)?;
    let input: ResyncBarrierInput =
        serde_json::from_value(value).map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let source_coverage = parse_source_coverage(input.source_coverage, context)?;
    let explicit_object_errors = parse_explicit_errors(input.explicit_object_errors, context)?;
    let replacement_manifest = ScopedReplacementManifestWire::from_wire_value_for_context(
        input.replacement_manifest,
        &context.replacement_manifest_context,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let capability_snapshot = ScopedCapabilitySnapshotWire::from_wire_value_for_context(
        input.capability_snapshot,
        &context.capability_context,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let scope_coverage = ScopedScopeCoverageWire::from_wire_value_for_context(
        input.scope_coverage,
        &context.scope_coverage_context,
    )
    .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let artifact_availability =
        ScopedArtifactAvailabilitySnapshotWire::from_wire_value_for_context(
            input.artifact_availability,
            &context.artifact_availability_context,
        )
        .map_err(ScopedCompletionEnvelopeContractError::nested)?;
    let ExpectedCompletionBarrier::Resync {
        started_control_sequence,
        coverage_snapshot_digest,
    } = context.expected_barrier
    else {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    };
    let replacement = decode_opaque_exact(
        &input.replacement_snapshot_digest,
        "resync replacement snapshot digest",
    )?;
    let coverage = decode_opaque_exact(
        &input.coverage_snapshot_digest,
        "resync coverage snapshot digest",
    )?;
    validate_positive_portable(
        "resync started control sequence",
        input.started_control_sequence,
    )?;
    input.queue_state.validate()?;
    if input.barrier_contract_version != SCOPED_RESYNC_BARRIER_CONTRACT_VERSION
        || input.scope_epoch != context.expected_scope_epoch
        || input.replacement != "full_snapshot"
        || input.started_control_sequence != started_control_sequence
        || input.started_control_sequence >= input.barrier_sequence
        || input.barrier_sequence != context.expected_observer_sequence
        || replacement != *context.expected_replacement_snapshot_digest.as_bytes()
        || coverage != *coverage_snapshot_digest.as_bytes()
        || input.queue_state != context.expected_queue_state
        || input.root_present != context.expected_root_present
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(ResyncBarrierWire {
        barrier_contract_version: input.barrier_contract_version,
        scope_epoch: input.scope_epoch,
        replacement: input.replacement,
        started_control_sequence: input.started_control_sequence,
        barrier_sequence: input.barrier_sequence,
        replacement_snapshot_digest: input.replacement_snapshot_digest,
        coverage_snapshot_digest: input.coverage_snapshot_digest,
        replacement_manifest,
        capability_snapshot,
        source_coverage,
        scope_coverage,
        explicit_object_errors,
        artifact_availability,
        queue_state: input.queue_state,
        root_present: input.root_present,
    })
}

fn parse_source_coverage(
    value: JsonValue,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> Result<Vec<SourceCoverageSet>, ScopedCompletionEnvelopeContractError> {
    let values = value.as_array().ok_or_else(|| {
        ScopedCompletionEnvelopeContractError::invalid(
            "completion source coverage must be an array",
        )
    })?;
    if values.len() != context.expected_source_coverage.len()
        || values.is_empty()
        || values.len() > MAX_SOURCE_COVERAGE_SETS
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    let parsed = values
        .iter()
        .cloned()
        .map(|value| {
            serde_json::from_value::<SourceCoverageSet>(value)
                .map_err(ScopedCompletionEnvelopeContractError::nested)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parsed != context.expected_source_coverage {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(parsed)
}

fn parse_explicit_errors(
    value: JsonValue,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> Result<Vec<CoverageError>, ScopedCompletionEnvelopeContractError> {
    let values = value.as_array().ok_or_else(|| {
        ScopedCompletionEnvelopeContractError::invalid(
            "completion explicit object errors must be an array",
        )
    })?;
    if values.len() != context.expected_explicit_object_errors.len()
        || values.len() > MAX_EXPLICIT_OBJECT_ERRORS
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    let parsed = values
        .iter()
        .cloned()
        .map(|value| {
            serde_json::from_value::<CoverageError>(value)
                .map_err(ScopedCompletionEnvelopeContractError::nested)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parsed != context.expected_explicit_object_errors {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(parsed)
}

fn canonical_explicit_errors(
    source_coverage: &[SourceCoverageSet],
) -> Result<Vec<CoverageError>, ScopedCompletionEnvelopeContractError> {
    let combined = source_coverage.iter().try_fold(0usize, |count, set| {
        count.checked_add(set.explicit_errors.len()).ok_or_else(|| {
            ScopedCompletionEnvelopeContractError::invalid(
                "completion explicit error count is exhausted",
            )
        })
    })?;
    if combined > MAX_EXPLICIT_OBJECT_ERRORS {
        return Err(ScopedCompletionEnvelopeContractError::invalid(
            "completion explicit object errors exceed the portable bound",
        ));
    }
    let mut errors = Vec::new();
    errors.try_reserve_exact(combined).map_err(|_| {
        ScopedCompletionEnvelopeContractError::invalid("completion error allocation failed")
    })?;
    for set in source_coverage {
        errors.extend(set.explicit_errors.iter().cloned());
    }
    errors.sort();
    errors.dedup();
    Ok(errors)
}

fn event_matches_context(
    event: &CompletionEventWire,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> bool {
    match (event, context.expected_barrier) {
        (
            CompletionEventWire::ObserverBootstrapComplete { barrier },
            ExpectedCompletionBarrier::Bootstrap { snapshot_digest },
        ) => {
            barrier.scope_epoch == context.expected_scope_epoch
                && barrier.barrier_sequence == context.expected_observer_sequence
                && barrier.snapshot_digest == encode_opaque(snapshot_digest.as_bytes())
                && barrier.replacement_snapshot_digest
                    == encode_opaque(context.expected_replacement_snapshot_digest.as_bytes())
        }
        (
            CompletionEventWire::ObserverResyncComplete { barrier },
            ExpectedCompletionBarrier::Resync {
                started_control_sequence,
                coverage_snapshot_digest,
            },
        ) => {
            barrier.scope_epoch == context.expected_scope_epoch
                && barrier.barrier_sequence == context.expected_observer_sequence
                && barrier.started_control_sequence == started_control_sequence
                && barrier.coverage_snapshot_digest
                    == encode_opaque(coverage_snapshot_digest.as_bytes())
                && barrier.replacement_snapshot_digest
                    == encode_opaque(context.expected_replacement_snapshot_digest.as_bytes())
        }
        _ => false,
    }
}

fn preflight_wire_value(
    value: &JsonValue,
    context: &ScopedCompletionEnvelopeConsumerContext,
) -> Result<(), ScopedCompletionEnvelopeContractError> {
    let object = exact_object(
        value,
        "completion envelope",
        &[
            "scoped_completion_envelope_contract_version",
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
    let event = object
        .get("event")
        .expect("checked completion event exists");
    let event_object = exact_object(event, "completion event", &["kind", "barrier"])?;
    preflight_barrier_arrays(
        event_object
            .get("barrier")
            .expect("checked completion barrier exists"),
        context,
        event_object.get("kind").and_then(JsonValue::as_str) == Some("observer_resync_complete"),
    )
}

fn preflight_barrier_arrays(
    value: &JsonValue,
    context: &ScopedCompletionEnvelopeConsumerContext,
    resync: bool,
) -> Result<(), ScopedCompletionEnvelopeContractError> {
    let fields: &[&str] = if resync {
        &[
            "barrier_contract_version",
            "scope_epoch",
            "replacement",
            "started_control_sequence",
            "barrier_sequence",
            "replacement_snapshot_digest",
            "coverage_snapshot_digest",
            "replacement_manifest",
            "capability_snapshot",
            "source_coverage",
            "scope_coverage",
            "explicit_object_errors",
            "artifact_availability",
            "queue_state",
            "root_present",
        ]
    } else {
        &[
            "barrier_contract_version",
            "scope_epoch",
            "barrier_sequence",
            "snapshot_digest",
            "replacement_snapshot_digest",
            "replacement_manifest",
            "capability_snapshot",
            "source_coverage",
            "scope_coverage",
            "explicit_object_errors",
            "artifact_availability",
            "queue_state",
            "root_present",
        ]
    };
    let barrier = exact_object(value, "completion barrier", fields)?;
    let coverage_count = barrier
        .get("source_coverage")
        .and_then(JsonValue::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            ScopedCompletionEnvelopeContractError::invalid(
                "completion source coverage must be an array",
            )
        })?;
    let error_count = barrier
        .get("explicit_object_errors")
        .and_then(JsonValue::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            ScopedCompletionEnvelopeContractError::invalid(
                "completion explicit object errors must be an array",
            )
        })?;
    if coverage_count != context.expected_source_coverage.len()
        || coverage_count == 0
        || coverage_count > MAX_SOURCE_COVERAGE_SETS
        || error_count != context.expected_explicit_object_errors.len()
        || error_count > MAX_EXPLICIT_OBJECT_ERRORS
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(())
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    fields: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedCompletionEnvelopeContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedCompletionEnvelopeContractError::invalid(format!("{label} must be an object"))
    })?;
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !fields.contains(&field.as_str()))
    {
        return Err(ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} fields do not match the exact contract"
        )));
    }
    Ok(object)
}

fn envelope_root_value(envelope: &ScopedObservationEnvelope) -> JsonValue {
    json!({
        "session_ref": envelope.root.session_ref,
        "session_key": envelope.root.session_key,
        "root_actor_run_key": envelope.root.root_actor_run_key,
        "native_session_claim": envelope.root.native_session_claim,
    })
}

fn root_wire_value(root: &ScopedObservationRootIdentity) -> JsonValue {
    json!({
        "session_ref": root.session_ref,
        "session_key": root.session_key,
        "root_actor_run_key": root.root_actor_run_key,
        "native_session_claim": root.native_session_claim,
    })
}

fn envelope_actor_value(envelope: &ScopedObservationEnvelope) -> JsonValue {
    json!({
        "root_session_key": envelope.actor.root_session_key,
        "run_key": envelope.actor.run_key,
        "role": match envelope.actor.role {
            ActorRunRole::Root => "root",
            ActorRunRole::Child => "child",
        },
        "parent_run_key": envelope.actor.parent_run_key,
        "native_session_id": envelope.actor.native_session_id,
        "native_actor_id": envelope.actor.native_actor_id,
        "native_actor_type": envelope.actor.native_actor_type,
    })
}

fn observer_attribution_value(
    envelope: &ScopedObservationEnvelope,
) -> Result<JsonValue, ScopedCompletionEnvelopeContractError> {
    if envelope.actor_attribution
        != (ScopedActorAttribution::ScopeFallback {
            reason: ScopedActorFallbackReason::ObserverLifecycleControl,
        })
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(json!({
        "kind": "scope_fallback",
        "reason": "observer_lifecycle_control",
    }))
}

fn envelope_affiliations_value(envelope: &ScopedObservationEnvelope) -> JsonValue {
    json!({
        "actor_run_key": envelope.affiliations.actor_run_key,
        "team_key": envelope.affiliations.team_key,
        "native_team_id": envelope.affiliations.native_team_id,
        "team_name": envelope.affiliations.team_name,
        "member_key": envelope.affiliations.member_key,
        "workflow_key": envelope.affiliations.workflow_key,
        "native_workflow_id": envelope.affiliations.native_workflow_id,
        "completeness": envelope.affiliations.completeness,
        "derived_from_revision_refs": envelope.affiliations.derived_from_revision_refs,
    })
}

fn envelope_source_value(
    envelope: &ScopedObservationEnvelope,
) -> Result<JsonValue, ScopedCompletionEnvelopeContractError> {
    if envelope.source.locator_id.is_some()
        || envelope.source.source_record_id.is_some()
        || envelope.source.record_index.is_some()
        || envelope.source.cursor_start.is_some()
        || envelope.source.cursor_end.is_some()
        || envelope.source.byte_range.is_some()
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(json!({
        "instance_key": envelope.source.instance_key,
        "stream_key": envelope.source.stream_key,
        "object_key": envelope.source.object_key,
        "locator_id": null,
        "generation": envelope.source.generation,
        "source_record_id": null,
        "record_index": null,
        "cursor_start": null,
        "cursor_end": null,
        "byte_range": null,
    }))
}

fn source_wire_value(source: &ScopedSourceObjectIdentity, generation: u64) -> JsonValue {
    json!({
        "instance_key": source.source_instance_key,
        "stream_key": source.stream_key,
        "object_key": source.object_key,
        "locator_id": null,
        "generation": generation,
        "source_record_id": null,
        "record_index": null,
        "cursor_start": null,
        "cursor_end": null,
        "byte_range": null,
    })
}

fn completion_evidence_value(
    envelope: &ScopedObservationEnvelope,
) -> Result<JsonValue, ScopedCompletionEnvelopeContractError> {
    if envelope.evidence.authority != ScopedEnvelopeEvidenceAuthority::EngineControl
        || envelope.evidence.quality != QualifiedValueQuality::Derived
        || envelope.evidence.effective_at.is_some()
        || envelope.evidence.completeness != ContractCompleteness::Complete
    {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(json!({
        "authority": "engine_control",
        "quality": "derived",
        "effective_at": null,
        "completeness": "complete",
    }))
}

fn completion_native_evidence_value(
    envelope: &ScopedObservationEnvelope,
) -> Result<JsonValue, ScopedCompletionEnvelopeContractError> {
    if !matches!(
        envelope.native_evidence,
        ScopedNativeEvidence::EngineControl
    ) {
        return Err(ScopedCompletionEnvelopeContractError::ContextMismatch);
    }
    Ok(json!({"kind": "engine_control"}))
}

fn validate_positive_portable(
    label: &str,
    value: u64,
) -> Result<(), ScopedCompletionEnvelopeContractError> {
    if value == 0 || value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} must be a positive portable integer"
        )));
    }
    Ok(())
}

fn validate_nonnegative_portable(
    label: &str,
    value: u64,
) -> Result<(), ScopedCompletionEnvelopeContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} must be a non-negative portable integer"
        )));
    }
    Ok(())
}

fn validate_portable_i64(
    label: &str,
    value: i64,
) -> Result<(), ScopedCompletionEnvelopeContractError> {
    if value < -(JS_SAFE_INTEGER_MAX as i64) || value > JS_SAFE_INTEGER_MAX as i64 {
        return Err(ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} must be a portable integer"
        )));
    }
    Ok(())
}

fn encode_opaque(bytes: &[u8; DIGEST_BYTES]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
) -> Result<[u8; DIGEST_BYTES], ScopedCompletionEnvelopeContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} must use the canonical opaque-reference prefix"
        ))
    })?;
    if encoded.len() != DIGEST_ENCODED_BYTES || encoded.contains('=') {
        return Err(ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} has invalid encoded length"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} is not canonical base64url"
        ))
    })?;
    let bytes: [u8; DIGEST_BYTES] = decoded.try_into().map_err(|_| {
        ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} must contain {DIGEST_BYTES} bytes"
        ))
    })?;
    if bytes.iter().all(|byte| *byte == 0) || URL_SAFE_NO_PAD.encode(bytes) != encoded {
        return Err(ScopedCompletionEnvelopeContractError::invalid(format!(
            "{label} is not canonical and nonzero"
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
