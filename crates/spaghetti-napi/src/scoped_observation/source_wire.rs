//! Strict portable RFC 012D wire projection for source lifecycle controls.
//!
//! This module freezes only source presence, reset, and object-error events.
//! Observer lifecycle controls, semantic fact events, and future typed-unknown
//! variants remain separate contracts. The top-level value has no unbound
//! `Deserialize` path: consumption requires the caller-held selection, root,
//! and authorized source set, and Rust recomputes the source-control event ID.

use std::collections::BTreeSet;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{
    ActorRunRole, CanonicalEntityKey, CanonicalSourceInstanceKey, ContractCompleteness,
    CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoveragePositionRef,
    CoverageStreamKey, ExternalEntityRef, NativeIdentityClaim, QualifiedTimestamp,
    QualifiedValueQuality, SemanticRevisionRef,
};
use crate::observation_contract::ObservationContractSelection;
use crate::source::AppendTransition;

use super::{
    source_object_error_event_id, source_presence_event_id, source_reset_event_id,
    ScopedActorAttribution, ScopedActorFallbackReason, ScopedAppendDeliveryPhase,
    ScopedAppendPresenceChange, ScopedAppendReset, ScopedEnvelopeEvidenceAuthority,
    ScopedNativeEvidence, ScopedObservationEnvelope, ScopedObservationEvent,
    ScopedObservationRootIdentity, ScopedSourceObjectError, ScopedSourceObjectErrorProvenance,
    ScopedSourceObjectFailureCode, ScopedSourceObjectIdentity, ScopedSourceObjectRetryState,
    SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION, SCOPED_SOURCE_OBJECT_ERROR_CONTRACT_VERSION,
};

pub(crate) const SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION: u32 = 1;

const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const MAX_AUTHORIZED_SOURCES: usize = 1_000;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedSourceEnvelopeContractError {
    #[error("invalid scoped source envelope contract: {message}")]
    Invalid { message: String },
    #[error("scoped envelope event is not supported by the source-control wire contract")]
    UnsupportedEvent,
}

impl ScopedSourceEnvelopeContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceDeliveryPhaseWire {
    Bootstrap,
    Live,
    Correction,
}

impl From<ScopedAppendDeliveryPhase> for SourceDeliveryPhaseWire {
    fn from(value: ScopedAppendDeliveryPhase) -> Self {
        match value {
            ScopedAppendDeliveryPhase::Bootstrap => Self::Bootstrap,
            ScopedAppendDeliveryPhase::Live => Self::Live,
            ScopedAppendDeliveryPhase::Correction => Self::Correction,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResetReasonWire {
    Truncated,
    IdentityChanged,
    PrefixMismatch,
    ContractReplay,
}

impl TryFrom<AppendTransition> for ResetReasonWire {
    type Error = ScopedSourceEnvelopeContractError;

    fn try_from(value: AppendTransition) -> Result<Self, Self::Error> {
        match value {
            AppendTransition::Truncated => Ok(Self::Truncated),
            AppendTransition::IdentityChanged => Ok(Self::IdentityChanged),
            AppendTransition::PrefixMismatch => Ok(Self::PrefixMismatch),
            AppendTransition::ContractReplay => Ok(Self::ContractReplay),
            AppendTransition::Initial | AppendTransition::Continued => {
                Err(ScopedSourceEnvelopeContractError::invalid(
                    "source reset requires a reset transition",
                ))
            }
        }
    }
}

impl From<ResetReasonWire> for AppendTransition {
    fn from(value: ResetReasonWire) -> Self {
        match value {
            ResetReasonWire::Truncated => Self::Truncated,
            ResetReasonWire::IdentityChanged => Self::IdentityChanged,
            ResetReasonWire::PrefixMismatch => Self::PrefixMismatch,
            ResetReasonWire::ContractReplay => Self::ContractReplay,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRootWire {
    session_ref: ExternalEntityRef,
    session_key: CanonicalEntityKey,
    root_actor_run_key: CanonicalEntityKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_claim: Option<NativeIdentityClaim>,
}

impl SourceRootWire {
    fn from_root(value: &ScopedObservationRootIdentity) -> Self {
        Self {
            session_ref: value.session_ref,
            session_key: value.session_key,
            root_actor_run_key: value.root_actor_run_key,
            native_session_claim: value.native_session_claim.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceActorRoleWire {
    Root,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceActorWire {
    root_session_key: CanonicalEntityKey,
    run_key: CanonicalEntityKey,
    role: SourceActorRoleWire,
    #[serde(deserialize_with = "deserialize_required_option")]
    parent_run_key: Option<CanonicalEntityKey>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_actor_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_actor_type: Option<String>,
}

impl SourceActorWire {
    fn from_root(value: &ScopedObservationRootIdentity) -> Self {
        Self {
            root_session_key: value.session_key,
            run_key: value.root_actor_run_key,
            role: SourceActorRoleWire::Root,
            parent_run_key: None,
            native_session_id: value
                .native_session_claim
                .as_ref()
                .and_then(|claim| claim.identity.value.as_ref())
                .map(|identity| identity.native_id.clone()),
            native_actor_id: None,
            native_actor_type: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceActorAttributionWire {
    kind: String,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAffiliationsWire {
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

impl SourceAffiliationsWire {
    fn from_root(value: &ScopedObservationRootIdentity) -> Self {
        Self {
            actor_run_key: value.root_actor_run_key,
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
struct SourceCoordinateWire {
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
    byte_range: Option<SourceByteRangeWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceByteRangeWire {
    start: u64,
    end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceEvidenceAuthorityWire {
    EngineControl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceEvidenceWire {
    authority: SourceEvidenceAuthorityWire,
    quality: QualifiedValueQuality,
    #[serde(deserialize_with = "deserialize_required_option")]
    effective_at: Option<QualifiedTimestamp>,
    completeness: ContractCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceFailureCodeWire {
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

impl From<ScopedSourceObjectFailureCode> for SourceFailureCodeWire {
    fn from(value: ScopedSourceObjectFailureCode) -> Self {
        match value {
            ScopedSourceObjectFailureCode::SourceRetryTransient => Self::SourceRetryTransient,
            ScopedSourceObjectFailureCode::SourceUnstable => Self::SourceUnstable,
            ScopedSourceObjectFailureCode::SourceDatabase => Self::SourceDatabase,
            ScopedSourceObjectFailureCode::SourceIo => Self::SourceIo,
            ScopedSourceObjectFailureCode::SourceInvalidConfiguration => {
                Self::SourceInvalidConfiguration
            }
            ScopedSourceObjectFailureCode::SourceInvalidCursor => Self::SourceInvalidCursor,
            ScopedSourceObjectFailureCode::SourcePathEscape => Self::SourcePathEscape,
            ScopedSourceObjectFailureCode::SourceLimitExceeded => Self::SourceLimitExceeded,
            ScopedSourceObjectFailureCode::DecodeRetryTransient => Self::DecodeRetryTransient,
            ScopedSourceObjectFailureCode::DecodeRecordPermanent => Self::DecodeRecordPermanent,
            ScopedSourceObjectFailureCode::DecodeStreamFatal => Self::DecodeStreamFatal,
        }
    }
}

impl From<SourceFailureCodeWire> for ScopedSourceObjectFailureCode {
    fn from(value: SourceFailureCodeWire) -> Self {
        match value {
            SourceFailureCodeWire::SourceRetryTransient => Self::SourceRetryTransient,
            SourceFailureCodeWire::SourceUnstable => Self::SourceUnstable,
            SourceFailureCodeWire::SourceDatabase => Self::SourceDatabase,
            SourceFailureCodeWire::SourceIo => Self::SourceIo,
            SourceFailureCodeWire::SourceInvalidConfiguration => Self::SourceInvalidConfiguration,
            SourceFailureCodeWire::SourceInvalidCursor => Self::SourceInvalidCursor,
            SourceFailureCodeWire::SourcePathEscape => Self::SourcePathEscape,
            SourceFailureCodeWire::SourceLimitExceeded => Self::SourceLimitExceeded,
            SourceFailureCodeWire::DecodeRetryTransient => Self::DecodeRetryTransient,
            SourceFailureCodeWire::DecodeRecordPermanent => Self::DecodeRecordPermanent,
            SourceFailureCodeWire::DecodeStreamFatal => Self::DecodeStreamFatal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceCoveragePositionKindWire {
    AppendCursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceCoveragePositionWire {
    kind: SourceCoveragePositionKindWire,
    opaque: CoveragePositionRef,
    monotonic_order: u64,
}

impl SourceCoveragePositionWire {
    fn from_internal(value: &CoveragePosition) -> Result<Self, ScopedSourceEnvelopeContractError> {
        if value.kind != CoveragePositionKind::AppendCursor {
            return Err(ScopedSourceEnvelopeContractError::invalid(
                "source object error position is not append-bound",
            ));
        }
        let monotonic_order = value.monotonic_order.ok_or_else(|| {
            ScopedSourceEnvelopeContractError::invalid(
                "source object error position lacks monotonic order",
            )
        })?;
        Ok(Self {
            kind: SourceCoveragePositionKindWire::AppendCursor,
            opaque: value.opaque,
            monotonic_order,
        })
    }

    fn to_internal(&self) -> CoveragePosition {
        CoveragePosition {
            kind: CoveragePositionKind::AppendCursor,
            opaque: self.opaque,
            monotonic_order: Some(self.monotonic_order),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceErrorProvenanceWire {
    generation: u64,
    #[serde(deserialize_with = "deserialize_required_option")]
    last_successful_position: Option<SourceCoveragePositionWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SourceRetryWire {
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

impl From<ScopedSourceObjectRetryState> for SourceRetryWire {
    fn from(value: ScopedSourceObjectRetryState) -> Self {
        match value {
            ScopedSourceObjectRetryState::RetryScheduled {
                failed_attempts,
                max_attempts,
                retry_after_ms,
            } => Self::RetryScheduled {
                failed_attempts,
                max_attempts,
                retry_after_ms,
            },
            ScopedSourceObjectRetryState::RetryExhausted {
                failed_attempts,
                max_attempts,
            } => Self::RetryExhausted {
                failed_attempts,
                max_attempts,
            },
            ScopedSourceObjectRetryState::NotRetryable { failed_attempts } => {
                Self::NotRetryable { failed_attempts }
            }
        }
    }
}

impl From<SourceRetryWire> for ScopedSourceObjectRetryState {
    fn from(value: SourceRetryWire) -> Self {
        match value {
            SourceRetryWire::RetryScheduled {
                failed_attempts,
                max_attempts,
                retry_after_ms,
            } => Self::RetryScheduled {
                failed_attempts,
                max_attempts,
                retry_after_ms,
            },
            SourceRetryWire::RetryExhausted {
                failed_attempts,
                max_attempts,
            } => Self::RetryExhausted {
                failed_attempts,
                max_attempts,
            },
            SourceRetryWire::NotRetryable { failed_attempts } => {
                Self::NotRetryable { failed_attempts }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceObjectErrorWire {
    error_contract_version: u32,
    relation_id: String,
    scope_epoch: u64,
    failure_code: SourceFailureCodeWire,
    provenance: SourceErrorProvenanceWire,
    retry: SourceRetryWire,
}

impl SourceObjectErrorWire {
    fn from_internal(
        value: &ScopedSourceObjectError,
    ) -> Result<Self, ScopedSourceEnvelopeContractError> {
        Ok(Self {
            error_contract_version: value.error_contract_version,
            relation_id: value.relation_id.to_string(),
            scope_epoch: value.scope_epoch,
            failure_code: value.failure_code.into(),
            provenance: SourceErrorProvenanceWire {
                generation: value.provenance.generation,
                last_successful_position: value
                    .provenance
                    .last_successful_position
                    .as_ref()
                    .map(SourceCoveragePositionWire::from_internal)
                    .transpose()?,
            },
            retry: value.retry.into(),
        })
    }

    fn to_internal(&self, source: ScopedSourceObjectIdentity) -> ScopedSourceObjectError {
        ScopedSourceObjectError {
            error_contract_version: self.error_contract_version,
            relation_id: Arc::from(self.relation_id.as_str()),
            source,
            scope_epoch: self.scope_epoch,
            failure_code: self.failure_code.into(),
            provenance: ScopedSourceObjectErrorProvenance {
                generation: self.provenance.generation,
                last_successful_position: self
                    .provenance
                    .last_successful_position
                    .as_ref()
                    .map(SourceCoveragePositionWire::to_internal),
            },
            retry: self.retry.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SourceControlEventWire {
    #[serde(rename = "source_created")]
    Created { generation: u64 },
    #[serde(rename = "source_deleted")]
    Deleted { generation: u64 },
    #[serde(rename = "source_reset")]
    Reset {
        old_generation: u64,
        new_generation: u64,
        reason: ResetReasonWire,
    },
    #[serde(rename = "source_object_error")]
    ObjectError { error: SourceObjectErrorWire },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceNativeEvidenceWire {
    kind: String,
}

/// Serialization-only v1 projection for one mapped source lifecycle event.
/// Received values must be consumed through `from_wire_value_for_context`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedSourceEnvelopeWire {
    scoped_source_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: ObservationContractSelection,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: Option<SemanticRevisionRef>,
    root: SourceRootWire,
    actor: SourceActorWire,
    actor_attribution: SourceActorAttributionWire,
    affiliations: SourceAffiliationsWire,
    source: SourceCoordinateWire,
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: SourceDeliveryPhaseWire,
    evidence: SourceEvidenceWire,
    event: SourceControlEventWire,
    native_evidence: SourceNativeEvidenceWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedSourceEnvelopeInput {
    scoped_source_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: JsonValue,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    semantic_revision_ref: Option<SemanticRevisionRef>,
    root: SourceRootWire,
    actor: SourceActorWire,
    actor_attribution: SourceActorAttributionWire,
    affiliations: SourceAffiliationsWire,
    source: SourceCoordinateWire,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: SourceDeliveryPhaseWire,
    evidence: SourceEvidenceWire,
    event: SourceControlEventWire,
    native_evidence: SourceNativeEvidenceWire,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl ScopedSourceEnvelopeWire {
    pub(crate) fn from_scoped_for_context(
        envelope: &ScopedObservationEnvelope,
        expected_root: &ScopedObservationRootIdentity,
        expected_sources: &[ScopedSourceObjectIdentity],
    ) -> Result<Self, ScopedSourceEnvelopeContractError> {
        let event = match &envelope.event {
            ScopedObservationEvent::SourcePresence { change } => match change {
                ScopedAppendPresenceChange::Created { generation } => {
                    SourceControlEventWire::Created {
                        generation: *generation,
                    }
                }
                ScopedAppendPresenceChange::Deleted { generation } => {
                    SourceControlEventWire::Deleted {
                        generation: *generation,
                    }
                }
            },
            ScopedObservationEvent::SourceReset { reset } => SourceControlEventWire::Reset {
                old_generation: reset.old_generation,
                new_generation: reset.new_generation,
                reason: reset.reason.try_into()?,
            },
            ScopedObservationEvent::SourceObjectError { error } => {
                SourceControlEventWire::ObjectError {
                    error: SourceObjectErrorWire::from_internal(error)?,
                }
            }
            _ => return Err(ScopedSourceEnvelopeContractError::UnsupportedEvent),
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
            || !matches!(
                envelope.native_evidence,
                ScopedNativeEvidence::EngineControl
            )
            || envelope.actor_attribution
                != (ScopedActorAttribution::ScopeFallback {
                    reason: ScopedActorFallbackReason::SourceLifecycleControl,
                })
        {
            return Err(ScopedSourceEnvelopeContractError::UnsupportedEvent);
        }
        let value = Self {
            scoped_source_envelope_contract_version: SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION,
            contract_version: envelope.contract_version,
            contract_selection: envelope.contract_selection.clone(),
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
            event_id: encode_opaque(envelope.event_id.as_bytes()),
            semantic_revision_ref: None,
            root: SourceRootWire {
                session_ref: envelope.root.session_ref,
                session_key: envelope.root.session_key,
                root_actor_run_key: envelope.root.root_actor_run_key,
                native_session_claim: envelope.root.native_session_claim.clone(),
            },
            actor: SourceActorWire {
                root_session_key: envelope.actor.root_session_key,
                run_key: envelope.actor.run_key,
                role: match envelope.actor.role {
                    ActorRunRole::Root => SourceActorRoleWire::Root,
                    ActorRunRole::Child => {
                        return Err(ScopedSourceEnvelopeContractError::UnsupportedEvent)
                    }
                },
                parent_run_key: envelope.actor.parent_run_key,
                native_session_id: envelope.actor.native_session_id.clone(),
                native_actor_id: envelope.actor.native_actor_id.clone(),
                native_actor_type: envelope.actor.native_actor_type.clone(),
            },
            actor_attribution: SourceActorAttributionWire {
                kind: "scope_fallback".to_owned(),
                reason: "source_lifecycle_control".to_owned(),
            },
            affiliations: SourceAffiliationsWire {
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
            source: SourceCoordinateWire {
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
            evidence: SourceEvidenceWire {
                authority: SourceEvidenceAuthorityWire::EngineControl,
                quality: envelope.evidence.quality,
                effective_at: envelope.evidence.effective_at.clone(),
                completeness: envelope.evidence.completeness,
            },
            event,
            native_evidence: SourceNativeEvidenceWire {
                kind: "engine_control".to_owned(),
            },
        };
        let source = value.validate_context(expected_root, expected_sources)?;
        value.validate_common(&source)?;
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        expected_selection: &ObservationContractSelection,
        expected_root: &ScopedObservationRootIdentity,
        expected_sources: &[ScopedSourceObjectIdentity],
    ) -> Result<Self, ScopedSourceEnvelopeContractError> {
        validate_raw_shape(&value, expected_root)?;
        let input: ScopedSourceEnvelopeInput =
            serde_json::from_value(value).map_err(|error| Self::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            expected_selection,
        )
        .map_err(|error| Self::invalid(error.to_string()))?;
        let value = Self {
            scoped_source_envelope_contract_version: input.scoped_source_envelope_contract_version,
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
        let source = value.validate_context(expected_root, expected_sources)?;
        value.validate_common(&source)?;
        Ok(value)
    }

    fn invalid(message: impl Into<String>) -> ScopedSourceEnvelopeContractError {
        ScopedSourceEnvelopeContractError::invalid(message)
    }

    fn validate_context(
        &self,
        expected_root: &ScopedObservationRootIdentity,
        expected_sources: &[ScopedSourceObjectIdentity],
    ) -> Result<ScopedSourceObjectIdentity, ScopedSourceEnvelopeContractError> {
        if self.root != SourceRootWire::from_root(expected_root) {
            return Err(Self::invalid(
                "source envelope does not match the caller-held root",
            ));
        }
        if expected_sources.is_empty() || expected_sources.len() > MAX_AUTHORIZED_SOURCES {
            return Err(Self::invalid(
                "source envelope is outside the caller-held authorized source set",
            ));
        }
        if expected_sources.iter().any(|source| {
            source.adapter_id != expected_root.adapter_id
                || source.source_instance_key != expected_root.source_instance_key
        }) {
            return Err(Self::invalid(
                "caller-held authorized source set crosses the exact observer root",
            ));
        }
        let unique = expected_sources
            .iter()
            .map(|source| {
                (
                    source.source_instance_key,
                    source.stream_key,
                    source.object_key,
                )
            })
            .collect::<BTreeSet<_>>();
        if unique.len() != expected_sources.len() {
            return Err(Self::invalid(
                "caller-held authorized source set contains duplicates",
            ));
        }
        expected_sources
            .iter()
            .find(|source| {
                source.source_instance_key == self.source.instance_key
                    && source.stream_key == self.source.stream_key
                    && source.object_key == self.source.object_key
            })
            .cloned()
            .ok_or_else(|| {
                Self::invalid("source envelope is outside the caller-held authorized source set")
            })
    }

    fn validate_common(
        &self,
        source: &ScopedSourceObjectIdentity,
    ) -> Result<(), ScopedSourceEnvelopeContractError> {
        if self.scoped_source_envelope_contract_version != SCOPED_SOURCE_ENVELOPE_CONTRACT_VERSION
            || self.contract_version != self.contract_selection.envelope_contract_version
            || self.contract_selection.event_contract_version
                != SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
        {
            return Err(Self::invalid(
                "source envelope does not match the selected source-control contract",
            ));
        }
        require_positive_safe(self.observer_sequence, "observer_sequence")?;
        require_positive_safe(self.scope_epoch, "scope_epoch")?;
        require_safe_i64(self.observed_at, "observed_at")?;
        require_positive_safe(self.source.generation, "source generation")?;
        if self.semantic_revision_ref.is_some()
            || self.native_time.is_some()
            || self.source.locator_id.is_some()
            || self.source.source_record_id.is_some()
            || self.source.record_index.is_some()
            || self.source.cursor_start.is_some()
            || self.source.cursor_end.is_some()
            || self.source.byte_range.is_some()
        {
            return Err(Self::invalid(
                "source lifecycle control contains record or native occurrence data",
            ));
        }
        if self.actor
            != SourceActorWire::from_root(&ScopedObservationRootIdentity {
                adapter_id: source.adapter_id.clone(),
                source_instance_key: self.source.instance_key,
                session_key: self.root.session_key,
                session_ref: self.root.session_ref,
                root_actor_run_key: self.root.root_actor_run_key,
                native_session_claim: self.root.native_session_claim.clone(),
            })
            || self.actor_attribution.kind != "scope_fallback"
            || self.actor_attribution.reason != "source_lifecycle_control"
        {
            return Err(Self::invalid(
                "source lifecycle actor is not the exact root fallback",
            ));
        }
        let expected_affiliations = SourceAffiliationsWire {
            actor_run_key: self.root.root_actor_run_key,
            team_key: None,
            native_team_id: None,
            team_name: None,
            member_key: None,
            workflow_key: None,
            native_workflow_id: None,
            completeness: ContractCompleteness::Unknown,
            derived_from_revision_refs: Vec::new(),
        };
        if self.affiliations != expected_affiliations {
            return Err(Self::invalid(
                "source lifecycle affiliations must remain explicitly unknown",
            ));
        }
        if self.evidence.authority != SourceEvidenceAuthorityWire::EngineControl
            || self.evidence.quality != QualifiedValueQuality::Derived
            || self.evidence.effective_at.is_some()
            || self.native_evidence.kind != "engine_control"
        {
            return Err(Self::invalid(
                "source lifecycle event has invalid engine-control evidence",
            ));
        }

        let expected_event_id = match &self.event {
            SourceControlEventWire::Created { generation } => {
                require_positive_safe(*generation, "created generation")?;
                if *generation != self.source.generation
                    || self.evidence.completeness != ContractCompleteness::Complete
                {
                    return Err(Self::invalid("source-created generation/evidence mismatch"));
                }
                source_presence_event_id(
                    source,
                    ScopedAppendPresenceChange::Created {
                        generation: *generation,
                    },
                )
            }
            SourceControlEventWire::Deleted { generation } => {
                require_positive_safe(*generation, "deleted generation")?;
                if *generation != self.source.generation
                    || self.evidence.completeness != ContractCompleteness::Complete
                {
                    return Err(Self::invalid("source-deleted generation/evidence mismatch"));
                }
                source_presence_event_id(
                    source,
                    ScopedAppendPresenceChange::Deleted {
                        generation: *generation,
                    },
                )
            }
            SourceControlEventWire::Reset {
                old_generation,
                new_generation,
                reason,
            } => {
                require_positive_safe(*old_generation, "reset old_generation")?;
                require_positive_safe(*new_generation, "reset new_generation")?;
                if old_generation.checked_add(1) != Some(*new_generation)
                    || *new_generation != self.source.generation
                    || self.phase != SourceDeliveryPhaseWire::Correction
                    || self.evidence.completeness != ContractCompleteness::Complete
                {
                    return Err(Self::invalid("source-reset lineage/evidence mismatch"));
                }
                source_reset_event_id(
                    source,
                    ScopedAppendReset {
                        old_generation: *old_generation,
                        new_generation: *new_generation,
                        reason: (*reason).into(),
                    },
                )
            }
            SourceControlEventWire::ObjectError { error } => {
                let error = error.to_internal(source.clone());
                let terminal = matches!(
                    error.retry,
                    ScopedSourceObjectRetryState::RetryExhausted { .. }
                        | ScopedSourceObjectRetryState::NotRetryable { .. }
                );
                if !error.validate()
                    || error.error_contract_version != SCOPED_SOURCE_OBJECT_ERROR_CONTRACT_VERSION
                    || error.scope_epoch != self.scope_epoch
                    || error.provenance.generation != self.source.generation
                    || self.phase != SourceDeliveryPhaseWire::Live
                    || self.evidence.completeness
                        != if terminal {
                            ContractCompleteness::Unknown
                        } else {
                            ContractCompleteness::Partial
                        }
                {
                    return Err(Self::invalid(
                        "source object error is internally inconsistent",
                    ));
                }
                if let Some(position) = &error.provenance.last_successful_position {
                    require_safe_u64(
                        position
                            .monotonic_order
                            .expect("validated source error position has monotonic order"),
                        "last successful position",
                    )?;
                }
                if let ScopedSourceObjectRetryState::RetryScheduled { retry_after_ms, .. } =
                    error.retry
                {
                    require_safe_u64(retry_after_ms, "retry_after_ms")?;
                }
                source_object_error_event_id(&error)
            }
        };
        if decode_opaque_exact(&self.event_id, "event_id", DIGEST_BYTES)?
            != expected_event_id.as_bytes()
        {
            return Err(Self::invalid(
                "event_id does not match the exact source lifecycle event",
            ));
        }
        Ok(())
    }
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    required: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedSourceEnvelopeContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedSourceEnvelopeContractError::invalid(format!("{label} must be an object"))
    })?;
    for field in required {
        if !object.contains_key(*field) {
            return Err(ScopedSourceEnvelopeContractError::invalid(format!(
                "{label} is missing field {field}"
            )));
        }
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()))
    {
        return Err(ScopedSourceEnvelopeContractError::invalid(format!(
            "{label} contains unknown field {field}"
        )));
    }
    Ok(object)
}

fn validate_raw_shape(
    value: &JsonValue,
    expected_root: &ScopedObservationRootIdentity,
) -> Result<(), ScopedSourceEnvelopeContractError> {
    let envelope = exact_object(
        value,
        "scoped source envelope",
        &[
            "scoped_source_envelope_contract_version",
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
    let expected_root_value = serde_json::to_value(SourceRootWire::from_root(expected_root))
        .expect("validated source root always serializes");
    if envelope["root"] != expected_root_value {
        return Err(ScopedSourceEnvelopeContractError::invalid(
            "source envelope does not exactly match the caller-held root",
        ));
    }
    let expected_actor_value = serde_json::to_value(SourceActorWire::from_root(expected_root))
        .expect("validated source actor always serializes");
    if envelope["actor"] != expected_actor_value {
        return Err(ScopedSourceEnvelopeContractError::invalid(
            "source envelope actor is not the caller-held root actor",
        ));
    }
    let expected_affiliations =
        serde_json::to_value(SourceAffiliationsWire::from_root(expected_root))
            .expect("validated source affiliations always serialize");
    if envelope["affiliations"] != expected_affiliations {
        return Err(ScopedSourceEnvelopeContractError::invalid(
            "source envelope affiliations are not explicitly unknown",
        ));
    }
    exact_object(
        &envelope["actor_attribution"],
        "source actor attribution",
        &["kind", "reason"],
    )?;
    exact_object(
        &envelope["source"],
        "source coordinate",
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
        "source evidence",
        &["authority", "quality", "effective_at", "completeness"],
    )?;
    exact_object(
        &envelope["native_evidence"],
        "source native evidence",
        &["kind"],
    )?;
    let event = envelope["event"].as_object().ok_or_else(|| {
        ScopedSourceEnvelopeContractError::invalid("source event must be an object")
    })?;
    match event.get("kind").and_then(JsonValue::as_str) {
        Some("source_created") | Some("source_deleted") => {
            exact_object(
                &envelope["event"],
                "source presence event",
                &["kind", "generation"],
            )?;
        }
        Some("source_reset") => {
            exact_object(
                &envelope["event"],
                "source reset event",
                &["kind", "old_generation", "new_generation", "reason"],
            )?;
        }
        Some("source_object_error") => {
            let event = exact_object(
                &envelope["event"],
                "source object error event",
                &["kind", "error"],
            )?;
            let error = exact_object(
                &event["error"],
                "source object error",
                &[
                    "error_contract_version",
                    "relation_id",
                    "scope_epoch",
                    "failure_code",
                    "provenance",
                    "retry",
                ],
            )?;
            let provenance = exact_object(
                &error["provenance"],
                "source object error provenance",
                &["generation", "last_successful_position"],
            )?;
            if !provenance["last_successful_position"].is_null() {
                exact_object(
                    &provenance["last_successful_position"],
                    "last successful position",
                    &["kind", "opaque", "monotonic_order"],
                )?;
            }
            let retry = error["retry"].as_object().ok_or_else(|| {
                ScopedSourceEnvelopeContractError::invalid("source retry state must be an object")
            })?;
            match retry.get("kind").and_then(JsonValue::as_str) {
                Some("retry_scheduled") => {
                    exact_object(
                        &error["retry"],
                        "scheduled source retry",
                        &["kind", "failed_attempts", "max_attempts", "retry_after_ms"],
                    )?;
                }
                Some("retry_exhausted") => {
                    exact_object(
                        &error["retry"],
                        "exhausted source retry",
                        &["kind", "failed_attempts", "max_attempts"],
                    )?;
                }
                Some("not_retryable") => {
                    exact_object(
                        &error["retry"],
                        "terminal source error",
                        &["kind", "failed_attempts"],
                    )?;
                }
                _ => {
                    return Err(ScopedSourceEnvelopeContractError::invalid(
                        "source object error has an unsupported retry state",
                    ));
                }
            }
        }
        _ => {
            return Err(ScopedSourceEnvelopeContractError::invalid(
                "source envelope has an unsupported event kind",
            ));
        }
    }
    Ok(())
}

fn encode_opaque(bytes: &[u8]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
    bytes: usize,
) -> Result<Vec<u8>, ScopedSourceEnvelopeContractError> {
    let encoded = value
        .strip_prefix(REFERENCE_PREFIX)
        .ok_or_else(|| ScopedSourceEnvelopeContractError::invalid(format!("{label} is not v1")))?;
    if encoded.is_empty() || encoded.contains('=') {
        return Err(ScopedSourceEnvelopeContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedSourceEnvelopeContractError::invalid(format!("{label} is not canonical base64url"))
    })?;
    if decoded.len() != bytes || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err(ScopedSourceEnvelopeContractError::invalid(format!(
            "{label} must contain exactly {bytes} bytes"
        )));
    }
    Ok(decoded)
}

fn require_positive_safe(value: u64, label: &str) -> Result<(), ScopedSourceEnvelopeContractError> {
    if value == 0 || value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedSourceEnvelopeContractError::invalid(format!(
            "{label} must be a positive portable integer"
        )));
    }
    Ok(())
}

fn require_safe_u64(value: u64, label: &str) -> Result<(), ScopedSourceEnvelopeContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedSourceEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable integer range"
        )));
    }
    Ok(())
}

fn require_safe_i64(value: i64, label: &str) -> Result<(), ScopedSourceEnvelopeContractError> {
    let max = JS_SAFE_INTEGER_MAX as i64;
    if !(-max..=max).contains(&value) {
        return Err(ScopedSourceEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable integer range"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
