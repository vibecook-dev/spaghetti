//! Strict contextual RFC 012D wire for one ordered artifact-availability event.
//!
//! The event is emitted only after the checked availability reducer and the
//! attachment-local ordered lane have accepted the same source occurrence.
//! Its caller-held context is minted while that private occurrence is still
//! available. The serialized context deliberately omits the verified source
//! declaration digest; Rust retains it to recompute event identity, while
//! portable consumers bind the exact Rust-issued event ID and expected entry.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::adapter::{ActorRunRole, ContractCompleteness, QualifiedValueQuality};
use crate::observation_contract::ObservationContractSelection;

use super::artifact_availability_wire::{
    preflight_entry_value, ScopedArtifactAvailabilityEntryWire,
};
use super::source_wire::{ScopedSourceEnvelopeWire, SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION};
use super::{
    artifact_availability_event_id_for_components, source_belongs_to_root,
    source_presence_event_id, ScopedActorAttribution, ScopedActorFallbackReason,
    ScopedAppendDeliveryPhase, ScopedAppendPresenceChange, ScopedDeliveredObservation,
    ScopedEnvelopeError, ScopedEnvelopeEvidenceAuthority, ScopedNativeEvidence,
    ScopedObservationEnvelope, ScopedObservationEvent, ScopedObservationEventId,
    ScopedObservationRootIdentity, ScopedProjectedObservation, ScopedSourceObjectIdentity,
    SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION,
};

pub(crate) const SCOPED_ARTIFACT_AVAILABILITY_ENVELOPE_CONTRACT_VERSION: u32 = 1;

const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const DIGEST_ENCODED_BYTES: usize = 43;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedArtifactAvailabilityEnvelopeContractError {
    #[error("invalid scoped artifact-availability envelope contract: {message}")]
    Invalid { message: String },
    #[error("scoped artifact-availability envelope does not match caller-held context")]
    ContextMismatch,
    #[error("scoped envelope event is not ordered artifact availability")]
    UnsupportedEvent,
}

impl ScopedArtifactAvailabilityEnvelopeContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactAvailabilityDeliveryPhaseWire {
    Bootstrap,
    Live,
}

impl TryFrom<ScopedAppendDeliveryPhase> for ArtifactAvailabilityDeliveryPhaseWire {
    type Error = ScopedArtifactAvailabilityEnvelopeContractError;

    fn try_from(value: ScopedAppendDeliveryPhase) -> Result<Self, Self::Error> {
        match value {
            ScopedAppendDeliveryPhase::Bootstrap => Ok(Self::Bootstrap),
            ScopedAppendDeliveryPhase::Live => Ok(Self::Live),
            ScopedAppendDeliveryPhase::Correction => {
                Err(ScopedArtifactAvailabilityEnvelopeContractError::UnsupportedEvent)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactAvailabilityEvidenceAuthorityWire {
    CommonReducer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactAvailabilityEvidenceWire {
    authority: ArtifactAvailabilityEvidenceAuthorityWire,
    quality: QualifiedValueQuality,
    #[serde(deserialize_with = "deserialize_required_option")]
    effective_at: Option<i64>,
    completeness: ContractCompleteness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactAvailabilityEventWire {
    kind: String,
    entry: ScopedArtifactAvailabilityEntryWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactAvailabilityNativeEvidenceWire {
    kind: String,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Exact, non-Serde authority retained beside one yielded availability event.
/// Debug withholds root/source/entry/declaration and event identity.
#[derive(Clone, PartialEq, Eq)]
pub struct ScopedArtifactAvailabilityEnvelopeConsumerContext {
    contract_selection: ObservationContractSelection,
    root: ScopedObservationRootIdentity,
    source: ScopedSourceObjectIdentity,
    source_declaration_digest: [u8; DIGEST_BYTES],
    source_generation: u64,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: ScopedObservationEventId,
    observed_at: i64,
    phase: ArtifactAvailabilityDeliveryPhaseWire,
    expected_entry: ScopedArtifactAvailabilityEntryWire,
}

impl std::fmt::Debug for ScopedArtifactAvailabilityEnvelopeConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedArtifactAvailabilityEnvelopeConsumerContext")
            .field("observer_sequence", &self.observer_sequence)
            .field("scope_epoch", &self.scope_epoch)
            .field("root", &"<redacted>")
            .field("source", &"<redacted>")
            .field("source_declaration_digest", &"sha256:<redacted>")
            .field("event_id", &"v1:<redacted>")
            .field("observed_at", &"<redacted>")
            .field("phase", &self.phase)
            .field("expected_entry", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ScopedArtifactAvailabilityEnvelopeConsumerContext {
    pub(super) fn from_delivered(
        contract_selection: &ObservationContractSelection,
        root: &ScopedObservationRootIdentity,
        delivered: &ScopedDeliveredObservation,
    ) -> Result<Option<Self>, ScopedEnvelopeError> {
        let ScopedProjectedObservation::ArtifactAvailability {
            occurrence,
            event_id,
            ..
        } = &delivered.event
        else {
            return Ok(None);
        };
        if delivered.event_contract_version != SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
            || delivered.observer_sequence == 0
            || delivered.observer_sequence > JS_SAFE_INTEGER_MAX
            || delivered.scope_epoch == 0
            || delivered.scope_epoch > JS_SAFE_INTEGER_MAX
            || !is_portable_i64(delivered.event.observed_at())
            || delivered.event_id != *event_id
            || delivered.source != *occurrence.source()
            || !source_belongs_to_root(occurrence.source(), root)
            || !occurrence.validate_for_root(root.session_key)
            || occurrence
                .source_declaration_digest()
                .iter()
                .all(|byte| *byte == 0)
            || *event_id != super::artifact_availability_event_id(occurrence)
        {
            return Err(ScopedEnvelopeError::DeliveryMismatch);
        }
        let expected_entry = ScopedArtifactAvailabilityEntryWire::from_internal(occurrence.entry());
        expected_entry
            .validate_shape()
            .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?;
        Ok(Some(Self {
            contract_selection: contract_selection.clone(),
            root: root.clone(),
            source: occurrence.source().clone(),
            source_declaration_digest: *occurrence.source_declaration_digest(),
            source_generation: occurrence.source_generation(),
            observer_sequence: delivered.observer_sequence,
            scope_epoch: delivered.scope_epoch,
            event_id: *event_id,
            observed_at: delivered.event.observed_at(),
            phase: delivered
                .phase
                .try_into()
                .map_err(|_| ScopedEnvelopeError::DeliveryMismatch)?,
            expected_entry,
        }))
    }

    pub(crate) fn wire(&self) -> ScopedArtifactAvailabilityEnvelopeContextWire {
        ScopedArtifactAvailabilityEnvelopeContextWire {
            contract_selection: self.contract_selection.clone(),
            root: root_wire_value(&self.root),
            expected_source: json!({
                "instance_key": self.source.source_instance_key,
                "stream_key": self.source.stream_key,
                "object_key": self.source.object_key,
                "generation": self.source_generation,
            }),
            expected_observer_sequence: self.observer_sequence,
            expected_scope_epoch: self.scope_epoch,
            expected_event_id: encode_opaque(self.event_id.as_bytes()),
            expected_observed_at: self.observed_at,
            expected_phase: self.phase,
            expected_entry: self.expected_entry.clone(),
        }
    }

    fn validate_self(&self) -> Result<(), ScopedArtifactAvailabilityEnvelopeContractError> {
        validate_positive_portable("expected observer_sequence", self.observer_sequence)?;
        validate_positive_portable("expected scope_epoch", self.scope_epoch)?;
        validate_positive_portable("expected source generation", self.source_generation)?;
        validate_portable_i64("expected observed_at", self.observed_at)?;
        self.expected_entry.validate_shape().map_err(|error| {
            ScopedArtifactAvailabilityEnvelopeContractError::invalid(error.to_string())
        })?;
        if !self
            .expected_entry
            .state
            .matches_source_generation(self.source_generation)
        {
            return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
                "artifact availability context state/source generation mismatch",
            ));
        }
        if self.source_declaration_digest.iter().all(|byte| *byte == 0)
            || !source_belongs_to_root(&self.source, &self.root)
        {
            return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
                "artifact availability context has invalid private source authority",
            ));
        }
        let expected_entry_revision =
            decode_opaque_exact(&self.expected_entry.revision, "expected entry revision")?;
        let expected_event_id = artifact_availability_event_id_for_components(
            &self.source,
            self.root.session_key,
            self.source_generation,
            &self.source_declaration_digest,
            self.expected_entry.artifact_key,
            &self.expected_entry.artifact_kind,
            &expected_entry_revision,
        );
        if self.event_id != expected_event_id {
            return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
                "artifact availability context event identity is inconsistent",
            ));
        }
        Ok(())
    }
}

/// Serialization-only portable caller context. The declaration digest is not
/// present; the exact expected event ID is the Rust-issued opaque commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedArtifactAvailabilityEnvelopeContextWire {
    contract_selection: ObservationContractSelection,
    root: JsonValue,
    expected_source: JsonValue,
    expected_observer_sequence: u64,
    expected_scope_epoch: u64,
    expected_event_id: String,
    expected_observed_at: i64,
    expected_phase: ArtifactAvailabilityDeliveryPhaseWire,
    expected_entry: ScopedArtifactAvailabilityEntryWire,
}

/// Serialization-only ordered event. Received values must pass the contextual
/// constructor and cannot establish their own source or availability state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedArtifactAvailabilityEnvelopeWire {
    scoped_artifact_availability_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: ObservationContractSelection,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: Option<JsonValue>,
    root: JsonValue,
    actor: JsonValue,
    actor_attribution: JsonValue,
    affiliations: JsonValue,
    source: JsonValue,
    native_time: Option<JsonValue>,
    observed_at: i64,
    phase: ArtifactAvailabilityDeliveryPhaseWire,
    evidence: ArtifactAvailabilityEvidenceWire,
    event: ArtifactAvailabilityEventWire,
    native_evidence: ArtifactAvailabilityNativeEvidenceWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedArtifactAvailabilityEnvelopeInput {
    scoped_artifact_availability_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: JsonValue,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    semantic_revision_ref: Option<JsonValue>,
    root: JsonValue,
    actor: JsonValue,
    actor_attribution: JsonValue,
    affiliations: JsonValue,
    source: JsonValue,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_time: Option<JsonValue>,
    observed_at: i64,
    phase: ArtifactAvailabilityDeliveryPhaseWire,
    evidence: ArtifactAvailabilityEvidenceWire,
    event: ArtifactAvailabilityEventWire,
    native_evidence: ArtifactAvailabilityNativeEvidenceWire,
}

impl ScopedArtifactAvailabilityEnvelopeWire {
    pub(crate) fn from_scoped_for_context(
        envelope: &ScopedObservationEnvelope,
        context: &ScopedArtifactAvailabilityEnvelopeConsumerContext,
    ) -> Result<Self, ScopedArtifactAvailabilityEnvelopeContractError> {
        let ScopedObservationEvent::ArtifactAvailability { entry } = &envelope.event else {
            return Err(ScopedArtifactAvailabilityEnvelopeContractError::UnsupportedEvent);
        };
        if envelope.semantic_revision_ref.is_some()
            || envelope.native_time.is_some()
            || envelope.source.locator_id.is_some()
            || envelope.source.source_record_id.is_some()
            || envelope.source.record_index.is_some()
            || envelope.source.cursor_start.is_some()
            || envelope.source.cursor_end.is_some()
            || envelope.source.byte_range.is_some()
            || envelope.evidence.effective_at.is_some()
            || !matches!(
                envelope.native_evidence,
                ScopedNativeEvidence::EngineControl
            )
        {
            return Err(ScopedArtifactAvailabilityEnvelopeContractError::UnsupportedEvent);
        }
        let wire = Self {
            scoped_artifact_availability_envelope_contract_version:
                SCOPED_ARTIFACT_AVAILABILITY_ENVELOPE_CONTRACT_VERSION,
            contract_version: envelope.contract_version,
            contract_selection: envelope.contract_selection.clone(),
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
            event_id: encode_opaque(envelope.event_id.as_bytes()),
            semantic_revision_ref: None,
            root: json!({
                "session_ref": envelope.root.session_ref,
                "session_key": envelope.root.session_key,
                "root_actor_run_key": envelope.root.root_actor_run_key,
                "native_session_claim": envelope.root.native_session_claim,
            }),
            actor: json!({
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
            }),
            actor_attribution: json!({
                "kind": match envelope.actor_attribution {
                    ScopedActorAttribution::ScopeFallback { .. } => "scope_fallback",
                    ScopedActorAttribution::NativeExact => "native_exact",
                    ScopedActorAttribution::DerivedExact => "derived_exact",
                },
                "reason": match envelope.actor_attribution {
                    ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::SourceLifecycleControl,
                    } => "source_lifecycle_control",
                    ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::ObserverLifecycleControl,
                    } => "observer_lifecycle_control",
                    ScopedActorAttribution::ScopeFallback {
                        reason: ScopedActorFallbackReason::UnknownWireEvent,
                    } => {
                        return Err(
                            ScopedArtifactAvailabilityEnvelopeContractError::UnsupportedEvent,
                        )
                    }
                    ScopedActorAttribution::NativeExact | ScopedActorAttribution::DerivedExact => "",
                },
            }),
            affiliations: json!({
                "actor_run_key": envelope.affiliations.actor_run_key,
                "team_key": envelope.affiliations.team_key,
                "native_team_id": envelope.affiliations.native_team_id,
                "team_name": envelope.affiliations.team_name,
                "member_key": envelope.affiliations.member_key,
                "workflow_key": envelope.affiliations.workflow_key,
                "native_workflow_id": envelope.affiliations.native_workflow_id,
                "completeness": envelope.affiliations.completeness,
                "derived_from_revision_refs": envelope.affiliations.derived_from_revision_refs,
            }),
            source: json!({
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
            }),
            native_time: None,
            observed_at: envelope.observed_at,
            phase: envelope.phase.try_into()?,
            evidence: ArtifactAvailabilityEvidenceWire {
                authority: match envelope.evidence.authority {
                    ScopedEnvelopeEvidenceAuthority::CommonReducer => {
                        ArtifactAvailabilityEvidenceAuthorityWire::CommonReducer
                    }
                    ScopedEnvelopeEvidenceAuthority::NativeRecord
                    | ScopedEnvelopeEvidenceAuthority::EngineControl
                    | ScopedEnvelopeEvidenceAuthority::PreservedUnknownWire => {
                        return Err(
                            ScopedArtifactAvailabilityEnvelopeContractError::UnsupportedEvent,
                        )
                    }
                },
                quality: envelope.evidence.quality,
                effective_at: None,
                completeness: envelope.evidence.completeness,
            },
            event: ArtifactAvailabilityEventWire {
                kind: "artifact_availability".to_owned(),
                entry: ScopedArtifactAvailabilityEntryWire::from_internal(entry),
            },
            native_evidence: ArtifactAvailabilityNativeEvidenceWire {
                kind: "engine_control".to_owned(),
            },
        };
        wire.validate_against(context)?;
        Ok(wire)
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &ScopedArtifactAvailabilityEnvelopeConsumerContext,
    ) -> Result<Self, ScopedArtifactAvailabilityEnvelopeContractError> {
        preflight_wire_value(&value)?;
        let input: ScopedArtifactAvailabilityEnvelopeInput = serde_json::from_value(value)
            .map_err(|error| {
                ScopedArtifactAvailabilityEnvelopeContractError::invalid(error.to_string())
            })?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            &context.contract_selection,
        )
        .map_err(|error| {
            ScopedArtifactAvailabilityEnvelopeContractError::invalid(error.to_string())
        })?;
        let wire = Self {
            scoped_artifact_availability_envelope_contract_version: input
                .scoped_artifact_availability_envelope_contract_version,
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
        wire.validate_against(context)?;
        Ok(wire)
    }

    fn validate_against(
        &self,
        context: &ScopedArtifactAvailabilityEnvelopeConsumerContext,
    ) -> Result<(), ScopedArtifactAvailabilityEnvelopeContractError> {
        context.validate_self()?;
        validate_positive_portable("observer_sequence", self.observer_sequence)?;
        validate_positive_portable("scope_epoch", self.scope_epoch)?;
        self.event.entry.validate_shape().map_err(|error| {
            ScopedArtifactAvailabilityEnvelopeContractError::invalid(error.to_string())
        })?;
        let expected_completeness = match &self.event.entry.state {
            super::artifact_availability_wire::ScopedArtifactAvailabilityStateWire::Unstable => {
                ContractCompleteness::Unknown
            }
            super::artifact_availability_wire::ScopedArtifactAvailabilityStateWire::Available {
                ..
            }
            | super::artifact_availability_wire::ScopedArtifactAvailabilityStateWire::Missing {
                ..
            }
            | super::artifact_availability_wire::ScopedArtifactAvailabilityStateWire::OverLimit {
                ..
            } => ContractCompleteness::Complete,
        };
        if self.scoped_artifact_availability_envelope_contract_version
            != SCOPED_ARTIFACT_AVAILABILITY_ENVELOPE_CONTRACT_VERSION
            || self.contract_version != self.contract_selection.envelope_contract_version
            || self.contract_selection != context.contract_selection
            || self.observer_sequence != context.observer_sequence
            || self.scope_epoch != context.scope_epoch
            || decode_opaque_exact(&self.event_id, "artifact availability event_id")?
                != *context.event_id.as_bytes()
            || self.observed_at != context.observed_at
            || self.phase != context.phase
            || self.semantic_revision_ref.is_some()
            || self.root != root_wire_value(&context.root)
            || self.source != source_wire_value(&context.source, context.source_generation)
            || self.native_time.is_some()
            || self.evidence.authority != ArtifactAvailabilityEvidenceAuthorityWire::CommonReducer
            || self.evidence.quality != QualifiedValueQuality::Derived
            || self.evidence.effective_at.is_some()
            || self.evidence.completeness != expected_completeness
            || self.event.kind != "artifact_availability"
            || self.event.entry != context.expected_entry
            || self.native_evidence.kind != "engine_control"
        {
            return Err(ScopedArtifactAvailabilityEnvelopeContractError::ContextMismatch);
        }
        self.validate_common_via_source_contract(context)
    }

    fn validate_common_via_source_contract(
        &self,
        context: &ScopedArtifactAvailabilityEnvelopeConsumerContext,
    ) -> Result<(), ScopedArtifactAvailabilityEnvelopeContractError> {
        let mut proxy = serde_json::to_value(self).map_err(|error| {
            ScopedArtifactAvailabilityEnvelopeContractError::invalid(error.to_string())
        })?;
        let object = proxy.as_object_mut().ok_or_else(|| {
            ScopedArtifactAvailabilityEnvelopeContractError::invalid(
                "artifact availability envelope must be an object",
            )
        })?;
        object.remove("scoped_artifact_availability_envelope_contract_version");
        object.insert(
            "scoped_source_envelope_contract_version".to_owned(),
            json!(SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION),
        );
        object.insert(
            "event_id".to_owned(),
            json!(encode_opaque(
                source_presence_event_id(
                    &context.source,
                    ScopedAppendPresenceChange::Created {
                        generation: context.source_generation,
                    },
                )
                .as_bytes(),
            )),
        );
        object.insert(
            "event".to_owned(),
            json!({
                "kind": "source_created",
                "generation": context.source_generation,
            }),
        );
        object.insert(
            "evidence".to_owned(),
            json!({
                "authority": "engine_control",
                "quality": "derived",
                "effective_at": null,
                "completeness": "complete",
            }),
        );
        ScopedSourceEnvelopeWire::from_wire_value_for_context(
            proxy,
            &context.contract_selection,
            &context.root,
            std::slice::from_ref(&context.source),
        )
        .map_err(|error| {
            ScopedArtifactAvailabilityEnvelopeContractError::invalid(error.to_string())
        })?;
        Ok(())
    }
}

fn root_wire_value(root: &ScopedObservationRootIdentity) -> JsonValue {
    json!({
        "session_ref": root.session_ref,
        "session_key": root.session_key,
        "root_actor_run_key": root.root_actor_run_key,
        "native_session_claim": root.native_session_claim,
    })
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

fn preflight_wire_value(
    value: &JsonValue,
) -> Result<(), ScopedArtifactAvailabilityEnvelopeContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedArtifactAvailabilityEnvelopeContractError::invalid(
            "artifact availability envelope must be an object",
        )
    })?;
    let fields = [
        "scoped_artifact_availability_envelope_contract_version",
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
    ];
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || !has_exact_reference_length(object.get("event_id"))
    {
        return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
            "artifact availability envelope has a missing, unknown, or oversized field",
        ));
    }
    let event = object
        .get("event")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| {
            ScopedArtifactAvailabilityEnvelopeContractError::invalid(
                "artifact availability event must be an object",
            )
        })?;
    if event.len() != 2
        || event.get("kind").and_then(JsonValue::as_str) != Some("artifact_availability")
        || !event.contains_key("entry")
    {
        return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
            "artifact availability event has an invalid shape",
        ));
    }
    preflight_entry_value(event.get("entry").expect("checked event entry exists")).map_err(
        |error| ScopedArtifactAvailabilityEnvelopeContractError::invalid(error.to_string()),
    )
}

fn has_exact_reference_length(value: Option<&JsonValue>) -> bool {
    value
        .and_then(JsonValue::as_str)
        .is_some_and(|value| value.len() == REFERENCE_PREFIX.len() + DIGEST_ENCODED_BYTES)
}

fn validate_positive_portable(
    label: &str,
    value: u64,
) -> Result<(), ScopedArtifactAvailabilityEnvelopeContractError> {
    if value == 0 || value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
            format!("{label} must be a positive portable integer"),
        ));
    }
    Ok(())
}

fn validate_portable_i64(
    label: &str,
    value: i64,
) -> Result<(), ScopedArtifactAvailabilityEnvelopeContractError> {
    if !is_portable_i64(value) {
        return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
            format!("{label} must be a portable integer"),
        ));
    }
    Ok(())
}

fn is_portable_i64(value: i64) -> bool {
    value >= -(JS_SAFE_INTEGER_MAX as i64) && value <= JS_SAFE_INTEGER_MAX as i64
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
) -> Result<[u8; DIGEST_BYTES], ScopedArtifactAvailabilityEnvelopeContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedArtifactAvailabilityEnvelopeContractError::invalid(format!(
            "{label} must use the canonical opaque-reference prefix",
        ))
    })?;
    if encoded.len() != DIGEST_ENCODED_BYTES || encoded.contains('=') {
        return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
            format!("{label} has invalid encoded length"),
        ));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedArtifactAvailabilityEnvelopeContractError::invalid(format!(
            "{label} is not canonical base64url",
        ))
    })?;
    let bytes: [u8; DIGEST_BYTES] = decoded.try_into().map_err(|_| {
        ScopedArtifactAvailabilityEnvelopeContractError::invalid(format!(
            "{label} must contain {DIGEST_BYTES} bytes",
        ))
    })?;
    if bytes.iter().all(|byte| *byte == 0) || URL_SAFE_NO_PAD.encode(bytes) != encoded {
        return Err(ScopedArtifactAvailabilityEnvelopeContractError::invalid(
            format!("{label} is not canonical and nonzero"),
        ));
    }
    Ok(bytes)
}

fn encode_opaque(bytes: &[u8; DIGEST_BYTES]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests;
