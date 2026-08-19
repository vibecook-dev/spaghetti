//! Pre-transport RFC 012D dispatch for every currently implemented event.
//!
//! The wrapper is intentionally named `Known`: event-contract v1 still lacks
//! the bounded `unknown_wire_event` carrier required for an additive complete
//! union. It is non-Serde, attachment-bound, and constructed from the delivery
//! preview before dequeue. Each known family is serialized only through its
//! already-frozen strict specialist contract.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::adapter::{
    CanonicalEntityKey, CanonicalSourceInstanceKey, CoverageObjectKey, CoverageStreamKey,
    ExternalEntityRef, NativeIdentityClaim,
};
use crate::observation_contract::ObservationContractSelection;

use super::actor_wire::ScopedActorEnvelopeWire;
use super::artifact_availability_event_wire::{
    ScopedArtifactAvailabilityEnvelopeConsumerContext, ScopedArtifactAvailabilityEnvelopeWire,
};
use super::completion_wire::{
    ScopedCompletionEnvelopeConsumerContext, ScopedCompletionEnvelopeWire,
};
use super::continuity_wire::{
    ScopedContinuityEnvelopeConsumerContext, ScopedContinuityEnvelopeWire,
};
use super::source_wire::ScopedSourceEnvelopeWire;
use super::usage_wire::ScopedUsageEnvelopeWire;
use super::{
    source_belongs_to_root, ScopedDeliveredObservation, ScopedEnvelopeError,
    ScopedObservationAttachmentAuthority, ScopedObservationEnvelope, ScopedObservationEvent,
    ScopedObservationRootIdentity,
};

pub(crate) const SCOPED_OBSERVATION_KNOWN_ENVELOPE_CONTRACT_VERSION: u32 = 1;
pub(crate) const SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedObservationKnownEventFamily {
    Usage,
    Actor,
    Source,
    ArtifactAvailability,
    Completion,
    Continuity,
}

impl ScopedObservationKnownEventFamily {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Actor => "actor",
            Self::Source => "source",
            Self::ArtifactAvailability => "artifact_availability",
            Self::Completion => "completion",
            Self::Continuity => "continuity",
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct KnownEnvelopeRootWire {
    session_ref: ExternalEntityRef,
    session_key: CanonicalEntityKey,
    root_actor_run_key: CanonicalEntityKey,
    native_session_claim: Option<NativeIdentityClaim>,
}

impl From<&ScopedObservationRootIdentity> for KnownEnvelopeRootWire {
    fn from(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            session_ref: root.session_ref,
            session_key: root.session_key,
            root_actor_run_key: root.root_actor_run_key,
            native_session_claim: root.native_session_claim.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct KnownEnvelopeSourceWire {
    instance_key: CanonicalSourceInstanceKey,
    stream_key: CoverageStreamKey,
    object_key: CoverageObjectKey,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct KnownEnvelopeContextWire {
    contract_selection: ObservationContractSelection,
    root: KnownEnvelopeRootWire,
    authorized_sources: Vec<KnownEnvelopeSourceWire>,
}

pub(super) struct ScopedSpecializedEnvelopeContexts<'a> {
    pub artifact_availability: Option<&'a ScopedArtifactAvailabilityEnvelopeConsumerContext>,
    pub completion: Option<&'a ScopedCompletionEnvelopeConsumerContext>,
    pub continuity: Option<&'a ScopedContinuityEnvelopeConsumerContext>,
}

/// One already-validated known envelope plus the caller context required by
/// its strict portable specialist. Neither JSON value is independently
/// authoritative: this process-local wrapper also retains the exact attachment
/// that owned the delivery preview.
pub(crate) struct ScopedObservationKnownEnvelope {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    family: ScopedObservationKnownEventFamily,
    wire_value: JsonValue,
    context_value: JsonValue,
    observer_sequence: u64,
    scope_epoch: u64,
}

impl std::fmt::Debug for ScopedObservationKnownEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationKnownEnvelope")
            .field("family", &self.family)
            .field("observer_sequence", &self.observer_sequence)
            .field("scope_epoch", &self.scope_epoch)
            .field("wire", &"<redacted>")
            .field("context", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ScopedObservationKnownEnvelope {
    pub(super) fn from_predequeue(
        envelope: &ScopedObservationEnvelope,
        expected_root: &ScopedObservationRootIdentity,
        attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
        delivered: &ScopedDeliveredObservation,
        contexts: ScopedSpecializedEnvelopeContexts<'_>,
    ) -> Result<Self, ScopedEnvelopeError> {
        validate_delivery(envelope, expected_root, delivered)?;
        let common_context = || {
            serde_json::to_value(KnownEnvelopeContextWire {
                contract_selection: envelope.contract_selection.clone(),
                root: expected_root.into(),
                authorized_sources: vec![KnownEnvelopeSourceWire {
                    instance_key: delivered.source.source_instance_key,
                    stream_key: delivered.source.stream_key,
                    object_key: delivered.source.object_key,
                }],
            })
            .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)
        };
        let (family, wire_value, context_value) = match &envelope.event {
            ScopedObservationEvent::UsageV2 { .. } => {
                require_no_specialized_contexts(&contexts)?;
                (
                    ScopedObservationKnownEventFamily::Usage,
                    serialize_wire(
                        ScopedUsageEnvelopeWire::from_scoped(envelope)
                            .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
                    )?,
                    common_context()?,
                )
            }
            ScopedObservationEvent::ActorRun { .. }
            | ScopedObservationEvent::ActorAffiliation { .. } => {
                require_no_specialized_contexts(&contexts)?;
                (
                    ScopedObservationKnownEventFamily::Actor,
                    serialize_wire(
                        ScopedActorEnvelopeWire::from_scoped(envelope)
                            .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
                    )?,
                    common_context()?,
                )
            }
            ScopedObservationEvent::SourcePresence { .. }
            | ScopedObservationEvent::SourceReset { .. }
            | ScopedObservationEvent::SourceObjectError { .. } => {
                require_no_specialized_contexts(&contexts)?;
                (
                    ScopedObservationKnownEventFamily::Source,
                    serialize_wire(
                        ScopedSourceEnvelopeWire::from_scoped_for_context(
                            envelope,
                            expected_root,
                            std::slice::from_ref(&delivered.source),
                        )
                        .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
                    )?,
                    common_context()?,
                )
            }
            ScopedObservationEvent::ArtifactAvailability { .. } => {
                let context = contexts
                    .artifact_availability
                    .ok_or(ScopedEnvelopeError::DeliveryMismatch)?;
                if contexts.completion.is_some() || contexts.continuity.is_some() {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                (
                    ScopedObservationKnownEventFamily::ArtifactAvailability,
                    serialize_wire(
                        ScopedArtifactAvailabilityEnvelopeWire::from_scoped_for_context(
                            envelope, context,
                        )
                        .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
                    )?,
                    serialize_wire(context.wire())?,
                )
            }
            ScopedObservationEvent::ObserverBootstrapComplete { .. }
            | ScopedObservationEvent::ObserverResyncComplete { .. } => {
                let context = contexts
                    .completion
                    .ok_or(ScopedEnvelopeError::DeliveryMismatch)?;
                if contexts.artifact_availability.is_some() || contexts.continuity.is_some() {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                (
                    ScopedObservationKnownEventFamily::Completion,
                    serialize_wire(
                        ScopedCompletionEnvelopeWire::from_scoped_for_context(envelope, context)
                            .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
                    )?,
                    serialize_wire(context.wire())?,
                )
            }
            ScopedObservationEvent::ObserverResyncRequired { .. }
            | ScopedObservationEvent::ObserverResyncStarted { .. }
            | ScopedObservationEvent::ObserverFailed { .. } => {
                let context = contexts
                    .continuity
                    .ok_or(ScopedEnvelopeError::DeliveryMismatch)?;
                if contexts.artifact_availability.is_some() || contexts.completion.is_some() {
                    return Err(ScopedEnvelopeError::DeliveryMismatch);
                }
                (
                    ScopedObservationKnownEventFamily::Continuity,
                    serialize_wire(
                        ScopedContinuityEnvelopeWire::from_scoped_for_context(
                            envelope,
                            &envelope.contract_selection,
                            expected_root,
                            context.state(),
                        )
                        .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
                    )?,
                    context
                        .wire_value()
                        .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
                )
            }
        };

        Ok(Self {
            attachment_authority,
            family,
            wire_value,
            context_value,
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
        })
    }

    pub(crate) fn family(&self) -> ScopedObservationKnownEventFamily {
        self.family
    }

    pub(crate) fn wire_value(&self) -> &JsonValue {
        &self.wire_value
    }

    pub(crate) fn context_value(&self) -> &JsonValue {
        &self.context_value
    }

    /// Strict outer dispatcher value for the currently known specialist
    /// contracts. Returning an owned value keeps this process-local authority
    /// non-Serde; a later native transport must still consume the yielded
    /// envelope under its attachment lifecycle.
    pub(crate) fn transport_value(&self) -> JsonValue {
        known_wire_value(
            self.family,
            self.context_value.clone(),
            self.wire_value.clone(),
        )
    }

    /// Complete outer-union shape for a currently known event. The same union
    /// also admits `unknown_wire_event` only when its separate preservation
    /// contract was negotiated; this runtime still has no internal unknown
    /// event variant and therefore emits only these checked known branches.
    pub(crate) fn event_union_value(&self) -> JsonValue {
        known_event_union_wire_value(
            self.family,
            self.context_value.clone(),
            self.wire_value.clone(),
        )
    }

    pub(super) fn belongs_to_attachment(
        &self,
        authority: &Arc<ScopedObservationAttachmentAuthority>,
    ) -> bool {
        Arc::ptr_eq(&self.attachment_authority, authority)
    }
}

fn known_wire_value(
    family: ScopedObservationKnownEventFamily,
    context: JsonValue,
    event: JsonValue,
) -> JsonValue {
    serde_json::json!({
        "scoped_known_envelope_contract_version":
            SCOPED_OBSERVATION_KNOWN_ENVELOPE_CONTRACT_VERSION,
        "family": family.wire_name(),
        "context": context,
        "event": event,
    })
}

fn known_event_union_wire_value(
    family: ScopedObservationKnownEventFamily,
    context: JsonValue,
    event: JsonValue,
) -> JsonValue {
    serde_json::json!({
        "scoped_observation_event_union_contract_version":
            SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION,
        "family": family.wire_name(),
        "context": context,
        "event": event,
    })
}

fn serialize_wire<T: Serialize>(wire: T) -> Result<JsonValue, ScopedEnvelopeError> {
    serde_json::to_value(wire).map_err(|_| ScopedEnvelopeError::DeliveryMismatch)
}

fn require_no_specialized_contexts(
    contexts: &ScopedSpecializedEnvelopeContexts<'_>,
) -> Result<(), ScopedEnvelopeError> {
    if contexts.artifact_availability.is_some()
        || contexts.completion.is_some()
        || contexts.continuity.is_some()
    {
        return Err(ScopedEnvelopeError::DeliveryMismatch);
    }
    Ok(())
}

fn validate_delivery(
    envelope: &ScopedObservationEnvelope,
    expected_root: &ScopedObservationRootIdentity,
    delivered: &ScopedDeliveredObservation,
) -> Result<(), ScopedEnvelopeError> {
    if delivered.event_contract_version != envelope.contract_selection.event_contract_version
        || envelope.contract_version != envelope.contract_selection.envelope_contract_version
        || envelope.observer_sequence != delivered.observer_sequence
        || envelope.scope_epoch != delivered.scope_epoch
        || envelope.event_id != delivered.event_id
        || envelope.semantic_revision_ref != delivered.semantic_revision_ref
        || envelope.phase != delivered.phase
        || envelope.observed_at != delivered.event.observed_at()
        || delivered.source != *delivered.event.source()
        || !source_belongs_to_root(&delivered.source, expected_root)
        || envelope.root.session_ref != expected_root.session_ref
        || envelope.root.session_key != expected_root.session_key
        || envelope.root.root_actor_run_key != expected_root.root_actor_run_key
        || envelope.root.native_session_claim != expected_root.native_session_claim
        || envelope.source.instance_key != delivered.source.source_instance_key
        || envelope.source.stream_key != delivered.source.stream_key
        || envelope.source.object_key != delivered.source.object_key
    {
        return Err(ScopedEnvelopeError::DeliveryMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
