//! Strict portable RFC 012D wire projection for the selected actor-run and
//! actor-affiliation event families.
//!
//! This remains a deliberately specialized, crate-private slice. It accepts
//! only the two already-reduced runtime families, withholds native payloads,
//! and requires caller-held negotiation, root, and source authority when
//! consuming a wire value. It neither authorizes source access nor exposes the
//! complete observation event union or an N-API transport.

use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{
    ActorAffiliationRevisionFact, ActorRunRevisionFact, ActorRunRole, CanonicalEntityKey,
    CanonicalFactId, CanonicalSourceInstanceKey, ContractCompleteness, CoverageObjectKey,
    CoverageStreamKey, ExternalEntityRef, FactRevisionId, FactSemanticRevision,
    NativeIdentityClaim, QualifiedTimestamp, QualifiedValueQuality, SemanticRevisionRef,
    SourceRecordId,
};
use crate::observation_contract::ObservationContractSelection;
use crate::source::{AppendTransition, SourceCursor, SourceRecordState};

use super::{
    revisioned_entity_event_id, scoped_actor_affiliation_context_is_valid,
    ScopedActorAffiliationContext, ScopedActorAttribution, ScopedAppendDeliveryPhase,
    ScopedAppendReset, ScopedEnvelopeEvidenceAuthority, ScopedNativeEvidence,
    ScopedNativeEvidenceWithheldReason, ScopedObservationEnvelope, ScopedObservationEvent,
    ScopedObservationRootIdentity, ScopedRevisionedEntityOperation,
    ScopedRevisionedEntityRetractionCause, ScopedSourceObjectIdentity,
    RUNTIME_ACTOR_AFFILIATION_FACT_FAMILY_CONTRACT_VERSION,
    RUNTIME_ACTOR_RUN_FACT_FAMILY_CONTRACT_VERSION,
};

pub(crate) const SCOPED_ACTOR_ENVELOPE_CONTRACT_VERSION: u32 = 1;

const ACTOR_RUN_FAMILY: &str = "runtime.actor-run";
const ACTOR_AFFILIATION_FAMILY: &str = "runtime.actor-affiliation";
const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const MAX_CURSOR_BYTES: usize = 128;
const MAX_MEDIA_TYPE_BYTES: usize = 256;
const MAX_RUNTIME_TEXT_BYTES: usize = 8 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_AFFILIATION_REVISIONS: usize = 64;
const MAX_AUTHORIZED_SOURCES: usize = 1_000;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedActorEnvelopeContractError {
    #[error("invalid scoped actor envelope contract: {message}")]
    Invalid { message: String },
    #[error("scoped envelope event is not supported by the actor wire contract")]
    UnsupportedEvent,
}

impl ScopedActorEnvelopeContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActorDeliveryPhaseWire {
    Bootstrap,
    Live,
    Correction,
}

impl From<ScopedAppendDeliveryPhase> for ActorDeliveryPhaseWire {
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
enum ActorOperationWire {
    Upsert,
    Retract,
}

impl From<ScopedRevisionedEntityOperation> for ActorOperationWire {
    fn from(value: ScopedRevisionedEntityOperation) -> Self {
        match value {
            ScopedRevisionedEntityOperation::Upsert => Self::Upsert,
            ScopedRevisionedEntityOperation::Retract => Self::Retract,
        }
    }
}

impl From<ActorOperationWire> for ScopedRevisionedEntityOperation {
    fn from(value: ActorOperationWire) -> Self {
        match value {
            ActorOperationWire::Upsert => Self::Upsert,
            ActorOperationWire::Retract => Self::Retract,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AppendTransitionWire {
    Truncated,
    IdentityChanged,
    PrefixMismatch,
    ContractReplay,
}

impl TryFrom<AppendTransition> for AppendTransitionWire {
    type Error = ScopedActorEnvelopeContractError;

    fn try_from(value: AppendTransition) -> Result<Self, Self::Error> {
        match value {
            AppendTransition::Truncated => Ok(Self::Truncated),
            AppendTransition::IdentityChanged => Ok(Self::IdentityChanged),
            AppendTransition::PrefixMismatch => Ok(Self::PrefixMismatch),
            AppendTransition::ContractReplay => Ok(Self::ContractReplay),
            AppendTransition::Initial | AppendTransition::Continued => {
                Err(ScopedActorEnvelopeContractError::invalid(
                    "actor retraction requires a reset transition",
                ))
            }
        }
    }
}

impl From<AppendTransitionWire> for AppendTransition {
    fn from(value: AppendTransitionWire) -> Self {
        match value {
            AppendTransitionWire::Truncated => Self::Truncated,
            AppendTransitionWire::IdentityChanged => Self::IdentityChanged,
            AppendTransitionWire::PrefixMismatch => Self::PrefixMismatch,
            AppendTransitionWire::ContractReplay => Self::ContractReplay,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ActorRetractionWire {
    Reset {
        old_generation: u64,
        new_generation: u64,
        reason: AppendTransitionWire,
    },
    SourceDeleted {
        generation: u64,
    },
}

impl ActorRetractionWire {
    fn from_internal(
        value: ScopedRevisionedEntityRetractionCause,
    ) -> Result<Self, ScopedActorEnvelopeContractError> {
        match value {
            ScopedRevisionedEntityRetractionCause::Reset(reset) => Ok(Self::Reset {
                old_generation: reset.old_generation,
                new_generation: reset.new_generation,
                reason: reset.reason.try_into()?,
            }),
            ScopedRevisionedEntityRetractionCause::SourceDeleted { generation } => {
                Ok(Self::SourceDeleted { generation })
            }
        }
    }

    fn to_internal(&self) -> ScopedRevisionedEntityRetractionCause {
        match *self {
            Self::Reset {
                old_generation,
                new_generation,
                reason,
            } => ScopedRevisionedEntityRetractionCause::Reset(ScopedAppendReset {
                old_generation,
                new_generation,
                reason: reason.into(),
            }),
            Self::SourceDeleted { generation } => {
                ScopedRevisionedEntityRetractionCause::SourceDeleted { generation }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorRootWire {
    session_ref: ExternalEntityRef,
    session_key: CanonicalEntityKey,
    root_actor_run_key: CanonicalEntityKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_claim: Option<NativeIdentityClaim>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorContextWire {
    root_session_key: CanonicalEntityKey,
    run_key: CanonicalEntityKey,
    role: ActorRunRole,
    #[serde(deserialize_with = "deserialize_required_option")]
    parent_run_key: Option<CanonicalEntityKey>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_actor_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_actor_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActorAttributionWire {
    DerivedExact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorAffiliationsWire {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorByteRangeWire {
    start: u64,
    end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorSourceWire {
    instance_key: CanonicalSourceInstanceKey,
    stream_key: CoverageStreamKey,
    object_key: CoverageObjectKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    locator_id: Option<String>,
    generation: u64,
    source_record_id: SourceRecordId,
    record_index: u32,
    cursor_start: String,
    cursor_end: String,
    byte_range: ActorByteRangeWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActorEvidenceAuthorityWire {
    NativeRecord,
    CommonReducer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorEvidenceWire {
    authority: ActorEvidenceAuthorityWire,
    quality: QualifiedValueQuality,
    #[serde(deserialize_with = "deserialize_required_option")]
    effective_at: Option<QualifiedTimestamp>,
    completeness: ContractCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceRecordStateWire {
    Present,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorNativeEvidenceWire {
    kind: String,
    media_type: String,
    state: SourceRecordStateWire,
    payload_hash: String,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ActorEventWire {
    ActorRun {
        fact_family: String,
        fact_family_contract_version: u32,
        fact_id: CanonicalFactId,
        operation: ActorOperationWire,
        #[serde(deserialize_with = "deserialize_required_option")]
        retraction: Option<ActorRetractionWire>,
        #[serde(deserialize_with = "deserialize_required_actor_run_revision")]
        revision: ActorRunRevisionFact,
    },
    ActorAffiliation {
        fact_family: String,
        fact_family_contract_version: u32,
        fact_id: CanonicalFactId,
        operation: ActorOperationWire,
        #[serde(deserialize_with = "deserialize_required_option")]
        retraction: Option<ActorRetractionWire>,
        #[serde(deserialize_with = "deserialize_required_affiliation_revision")]
        revision: ActorAffiliationRevisionFact,
    },
}

/// Serialization-only v1 projection of one selected actor or affiliation
/// event. Contextual consumption is mandatory; this type has no unbound
/// `Deserialize` implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedActorEnvelopeWire {
    scoped_actor_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: ObservationContractSelection,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: SemanticRevisionRef,
    root: ActorRootWire,
    actor: ActorContextWire,
    actor_attribution: ActorAttributionWire,
    affiliations: ActorAffiliationsWire,
    source: ActorSourceWire,
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: ActorDeliveryPhaseWire,
    evidence: ActorEvidenceWire,
    event: ActorEventWire,
    native_evidence: ActorNativeEvidenceWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedActorEnvelopeInput {
    scoped_actor_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: JsonValue,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: SemanticRevisionRef,
    root: ActorRootWire,
    actor: ActorContextWire,
    actor_attribution: ActorAttributionWire,
    affiliations: ActorAffiliationsWire,
    source: ActorSourceWire,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: ActorDeliveryPhaseWire,
    evidence: ActorEvidenceWire,
    event: ActorEventWire,
    native_evidence: ActorNativeEvidenceWire,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn deserialize_required_actor_run_revision<'de, D>(
    deserializer: D,
) -> Result<ActorRunRevisionFact, D::Error>
where
    D: Deserializer<'de>,
{
    let value = JsonValue::deserialize(deserializer)?;
    validate_actor_run_revision_shape(&value).map_err(D::Error::custom)?;
    serde_json::from_value(value).map_err(D::Error::custom)
}

fn deserialize_required_affiliation_revision<'de, D>(
    deserializer: D,
) -> Result<ActorAffiliationRevisionFact, D::Error>
where
    D: Deserializer<'de>,
{
    let value = JsonValue::deserialize(deserializer)?;
    validate_affiliation_revision_shape(&value).map_err(D::Error::custom)?;
    serde_json::from_value(value).map_err(D::Error::custom)
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedActorEnvelopeContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedActorEnvelopeContractError::invalid(format!("{label} must be an object"))
    })?;
    for field in required {
        if !object.contains_key(*field) {
            return Err(ScopedActorEnvelopeContractError::invalid(format!(
                "{label} is missing field {field}"
            )));
        }
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} contains unknown field {field}"
        )));
    }
    Ok(object)
}

fn validate_semantic_ref_shape(
    value: &JsonValue,
    label: &str,
) -> Result<(), ScopedActorEnvelopeContractError> {
    exact_object(
        value,
        label,
        &["semantic_reference_contract_version", "fact_revision_id"],
        &[],
    )?;
    Ok(())
}

fn validate_timestamp_shape(
    value: &JsonValue,
    label: &str,
) -> Result<(), ScopedActorEnvelopeContractError> {
    if !value.is_null() {
        exact_object(value, label, &["value", "quality"], &[])?;
    }
    Ok(())
}

fn validate_native_claim_shape(value: &JsonValue) -> Result<(), ScopedActorEnvelopeContractError> {
    if value.is_null() {
        return Ok(());
    }
    let claim = exact_object(
        value,
        "native session claim",
        &["entity_ref", "identity"],
        &[],
    )?;
    let identity = exact_object(
        &claim["identity"],
        "native session qualified identity",
        &[
            "value",
            "quality",
            "authority",
            "completeness",
            "provenance",
        ],
        &["unknown_reason", "effective_at"],
    )?;
    if !identity["value"].is_null() {
        exact_object(
            &identity["value"],
            "native session identity",
            &["native_namespace", "native_id"],
            &[],
        )?;
    }
    let provenance = identity["provenance"].as_array().ok_or_else(|| {
        ScopedActorEnvelopeContractError::invalid(
            "native session claim provenance must be an array",
        )
    })?;
    if provenance.len() > MAX_AFFILIATION_REVISIONS {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "native session claim provenance exceeds its bound",
        ));
    }
    for reference in provenance {
        validate_semantic_ref_shape(reference, "native session claim provenance")?;
    }
    Ok(())
}

fn validate_actor_run_revision_shape(
    value: &JsonValue,
) -> Result<(), ScopedActorEnvelopeContractError> {
    exact_object(
        value,
        "actor-run revision",
        &[
            "actor_run",
            "session",
            "role",
            "parent_actor_run",
            "native_session_id",
            "native_actor_id",
            "native_actor_type",
        ],
        &[],
    )?;
    Ok(())
}

fn validate_affiliation_revision_shape(
    value: &JsonValue,
) -> Result<(), ScopedActorEnvelopeContractError> {
    let revision = exact_object(
        value,
        "actor-affiliation revision",
        &[
            "affiliation",
            "actor_run",
            "session",
            "dimension",
            "target",
            "member",
            "native_target_id",
            "native_member_id",
            "state",
            "effective_at",
        ],
        &[],
    )?;
    validate_timestamp_shape(&revision["effective_at"], "actor-affiliation effective_at")
}

fn validate_retraction_shape(value: &JsonValue) -> Result<(), ScopedActorEnvelopeContractError> {
    if value.is_null() {
        return Ok(());
    }
    let retraction = value.as_object().ok_or_else(|| {
        ScopedActorEnvelopeContractError::invalid("actor retraction must be an object")
    })?;
    match retraction.get("kind").and_then(JsonValue::as_str) {
        Some("reset") => {
            exact_object(
                value,
                "actor reset retraction",
                &["kind", "old_generation", "new_generation", "reason"],
                &[],
            )?;
        }
        Some("source_deleted") => {
            exact_object(
                value,
                "actor deletion retraction",
                &["kind", "generation"],
                &[],
            )?;
        }
        _ => {
            return Err(ScopedActorEnvelopeContractError::invalid(
                "actor retraction has an unsupported kind",
            ))
        }
    }
    Ok(())
}

fn validate_actor_envelope_raw_shape(
    value: &JsonValue,
) -> Result<(), ScopedActorEnvelopeContractError> {
    let envelope = exact_object(
        value,
        "scoped actor envelope",
        &[
            "scoped_actor_envelope_contract_version",
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
        &[],
    )?;
    validate_semantic_ref_shape(&envelope["semantic_revision_ref"], "semantic revision ref")?;
    let root = exact_object(
        &envelope["root"],
        "actor root",
        &[
            "session_ref",
            "session_key",
            "root_actor_run_key",
            "native_session_claim",
        ],
        &[],
    )?;
    validate_native_claim_shape(&root["native_session_claim"])?;
    exact_object(
        &envelope["actor"],
        "actor context",
        &[
            "root_session_key",
            "run_key",
            "role",
            "parent_run_key",
            "native_session_id",
            "native_actor_id",
            "native_actor_type",
        ],
        &[],
    )?;
    let affiliations = exact_object(
        &envelope["affiliations"],
        "actor affiliations",
        &[
            "actor_run_key",
            "team_key",
            "native_team_id",
            "team_name",
            "member_key",
            "workflow_key",
            "native_workflow_id",
            "completeness",
            "derived_from_revision_refs",
        ],
        &[],
    )?;
    let affiliation_refs = affiliations["derived_from_revision_refs"]
        .as_array()
        .ok_or_else(|| {
            ScopedActorEnvelopeContractError::invalid(
                "actor affiliation revision refs must be an array",
            )
        })?;
    if affiliation_refs.len() > MAX_AFFILIATION_REVISIONS {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "actor affiliation revision refs exceed their bound",
        ));
    }
    for reference in affiliation_refs {
        validate_semantic_ref_shape(reference, "actor affiliation revision ref")?;
    }
    let source = exact_object(
        &envelope["source"],
        "actor source",
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
        &[],
    )?;
    exact_object(
        &source["byte_range"],
        "actor byte range",
        &["start", "end"],
        &[],
    )?;
    validate_timestamp_shape(&envelope["native_time"], "actor native time")?;
    let evidence = exact_object(
        &envelope["evidence"],
        "actor evidence",
        &["authority", "quality", "effective_at", "completeness"],
        &[],
    )?;
    validate_timestamp_shape(&evidence["effective_at"], "actor evidence effective_at")?;
    let event = exact_object(
        &envelope["event"],
        "actor event",
        &[
            "kind",
            "fact_family",
            "fact_family_contract_version",
            "fact_id",
            "operation",
            "retraction",
            "revision",
        ],
        &[],
    )?;
    validate_retraction_shape(&event["retraction"])?;
    match event["kind"].as_str() {
        Some("actor_run") => validate_actor_run_revision_shape(&event["revision"])?,
        Some("actor_affiliation") => validate_affiliation_revision_shape(&event["revision"])?,
        _ => {
            return Err(ScopedActorEnvelopeContractError::invalid(
                "actor event has an unsupported kind",
            ))
        }
    }
    exact_object(
        &envelope["native_evidence"],
        "actor native evidence",
        &["kind", "media_type", "state", "payload_hash", "reason"],
        &[],
    )?;
    Ok(())
}

impl ScopedActorEnvelopeWire {
    pub(crate) fn from_scoped(
        envelope: &ScopedObservationEnvelope,
    ) -> Result<Self, ScopedActorEnvelopeContractError> {
        let event = match &envelope.event {
            ScopedObservationEvent::ActorRun {
                fact_id,
                operation,
                retraction,
                revision,
            } => ActorEventWire::ActorRun {
                fact_family: ACTOR_RUN_FAMILY.to_owned(),
                fact_family_contract_version: RUNTIME_ACTOR_RUN_FACT_FAMILY_CONTRACT_VERSION,
                fact_id: *fact_id,
                operation: (*operation).into(),
                retraction: retraction
                    .map(ActorRetractionWire::from_internal)
                    .transpose()?,
                revision: (**revision).clone(),
            },
            ScopedObservationEvent::ActorAffiliation {
                fact_id,
                operation,
                retraction,
                revision,
            } => ActorEventWire::ActorAffiliation {
                fact_family: ACTOR_AFFILIATION_FAMILY.to_owned(),
                fact_family_contract_version:
                    RUNTIME_ACTOR_AFFILIATION_FACT_FAMILY_CONTRACT_VERSION,
                fact_id: *fact_id,
                operation: (*operation).into(),
                retraction: retraction
                    .map(ActorRetractionWire::from_internal)
                    .transpose()?,
                revision: (**revision).clone(),
            },
            _ => return Err(ScopedActorEnvelopeContractError::UnsupportedEvent),
        };
        let ScopedNativeEvidence::Withheld {
            media_type,
            state,
            payload_hash,
            reason,
        } = &envelope.native_evidence
        else {
            return Err(ScopedActorEnvelopeContractError::UnsupportedEvent);
        };
        if *state != SourceRecordState::Present
            || *reason != ScopedNativeEvidenceWithheldReason::ProjectionBoundary
            || envelope.actor_attribution != ScopedActorAttribution::DerivedExact
        {
            return Err(ScopedActorEnvelopeContractError::UnsupportedEvent);
        }
        let source_record_id = envelope
            .source
            .source_record_id
            .ok_or_else(|| Self::invalid("actor source is missing source_record_id"))?;
        let record_index = envelope
            .source
            .record_index
            .ok_or_else(|| Self::invalid("actor source is missing record_index"))?;
        let cursor_start = envelope
            .source
            .cursor_start
            .as_ref()
            .ok_or_else(|| Self::invalid("actor source is missing cursor_start"))?;
        let cursor_end = envelope
            .source
            .cursor_end
            .as_ref()
            .ok_or_else(|| Self::invalid("actor source is missing cursor_end"))?;
        let byte_range = envelope
            .source
            .byte_range
            .ok_or_else(|| Self::invalid("actor source is missing byte_range"))?;
        let semantic_revision_ref = envelope
            .semantic_revision_ref
            .ok_or_else(|| Self::invalid("actor envelope is missing semantic_revision_ref"))?;
        let authority = match envelope.evidence.authority {
            ScopedEnvelopeEvidenceAuthority::NativeRecord => {
                ActorEvidenceAuthorityWire::NativeRecord
            }
            ScopedEnvelopeEvidenceAuthority::CommonReducer => {
                ActorEvidenceAuthorityWire::CommonReducer
            }
            ScopedEnvelopeEvidenceAuthority::EngineControl => {
                return Err(ScopedActorEnvelopeContractError::UnsupportedEvent)
            }
        };
        let value = Self {
            scoped_actor_envelope_contract_version: SCOPED_ACTOR_ENVELOPE_CONTRACT_VERSION,
            contract_version: envelope.contract_version,
            contract_selection: envelope.contract_selection.clone(),
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
            event_id: encode_opaque(envelope.event_id.as_bytes()),
            semantic_revision_ref,
            root: ActorRootWire {
                session_ref: envelope.root.session_ref,
                session_key: envelope.root.session_key,
                root_actor_run_key: envelope.root.root_actor_run_key,
                native_session_claim: envelope.root.native_session_claim.clone(),
            },
            actor: ActorContextWire {
                root_session_key: envelope.actor.root_session_key,
                run_key: envelope.actor.run_key,
                role: envelope.actor.role,
                parent_run_key: envelope.actor.parent_run_key,
                native_session_id: envelope.actor.native_session_id.clone(),
                native_actor_id: envelope.actor.native_actor_id.clone(),
                native_actor_type: envelope.actor.native_actor_type.clone(),
            },
            actor_attribution: ActorAttributionWire::DerivedExact,
            affiliations: ActorAffiliationsWire {
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
            source: ActorSourceWire {
                instance_key: envelope.source.instance_key,
                stream_key: envelope.source.stream_key,
                object_key: envelope.source.object_key,
                locator_id: envelope.source.locator_id.clone(),
                generation: envelope.source.generation,
                source_record_id,
                record_index,
                cursor_start: encode_opaque(cursor_start.as_bytes()),
                cursor_end: encode_opaque(cursor_end.as_bytes()),
                byte_range: ActorByteRangeWire {
                    start: byte_range.start,
                    end: byte_range.end,
                },
            },
            native_time: envelope.native_time.clone(),
            observed_at: envelope.observed_at,
            phase: envelope.phase.into(),
            evidence: ActorEvidenceWire {
                authority,
                quality: envelope.evidence.quality,
                effective_at: envelope.evidence.effective_at.clone(),
                completeness: envelope.evidence.completeness,
            },
            event,
            native_evidence: ActorNativeEvidenceWire {
                kind: "withheld".to_owned(),
                media_type: media_type.as_str().to_owned(),
                state: SourceRecordStateWire::Present,
                payload_hash: encode_opaque(payload_hash.as_bytes()),
                reason: "projection_boundary".to_owned(),
            },
        };
        value.validate_common()?;
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        expected_selection: &ObservationContractSelection,
        expected_root: &ScopedObservationRootIdentity,
        expected_sources: &[ScopedSourceObjectIdentity],
    ) -> Result<Self, ScopedActorEnvelopeContractError> {
        validate_actor_envelope_raw_shape(&value)?;
        let input: ScopedActorEnvelopeInput =
            serde_json::from_value(value).map_err(|error| Self::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            expected_selection,
        )
        .map_err(|error| Self::invalid(error.to_string()))?;
        let value = Self {
            scoped_actor_envelope_contract_version: input.scoped_actor_envelope_contract_version,
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
        value.validate_common()?;
        value.validate_context(expected_root, expected_sources)?;
        Ok(value)
    }

    fn invalid(message: impl Into<String>) -> ScopedActorEnvelopeContractError {
        ScopedActorEnvelopeContractError::invalid(message)
    }

    fn validate_common(&self) -> Result<(), ScopedActorEnvelopeContractError> {
        if self.scoped_actor_envelope_contract_version != SCOPED_ACTOR_ENVELOPE_CONTRACT_VERSION
            || self.contract_version != self.contract_selection.envelope_contract_version
        {
            return Err(Self::invalid(
                "actor envelope does not match the selected v1 envelope contract",
            ));
        }
        require_positive_safe(self.observer_sequence, "observer_sequence")?;
        require_positive_safe(self.scope_epoch, "scope_epoch")?;
        require_safe_i64(self.observed_at, "observed_at")?;
        validate_nonzero(self.root.session_key.as_bytes(), "root session key")?;
        validate_nonzero(
            self.root.root_actor_run_key.as_bytes(),
            "root actor-run key",
        )?;
        if self.root.session_ref.entity_key != self.root.session_key
            || self.actor.root_session_key != self.root.session_key
            || self.affiliations.actor_run_key != self.actor.run_key
        {
            return Err(Self::invalid(
                "actor root, actor context, and affiliation context disagree",
            ));
        }
        let expected_role = if self.actor.run_key == self.root.root_actor_run_key {
            ActorRunRole::Root
        } else {
            ActorRunRole::Child
        };
        if self.actor.role != expected_role {
            return Err(Self::invalid(
                "actor role does not match the envelope root actor",
            ));
        }
        validate_native_claim(&self.root)?;
        validate_actor_context(&self.actor, &self.root, &self.contract_selection)?;
        validate_affiliations(
            &self.affiliations,
            &self.contract_selection,
            self.actor.run_key,
        )?;
        validate_source(&self.source)?;
        validate_timestamp(self.native_time.as_ref(), "native_time")?;
        validate_timestamp(self.evidence.effective_at.as_ref(), "evidence effective_at")?;

        let (
            family,
            family_version,
            fact_id,
            operation,
            retraction,
            session,
            actor_run,
            effective_at,
            revision_key,
        ) = match &self.event {
            ActorEventWire::ActorRun {
                fact_family,
                fact_family_contract_version,
                fact_id,
                operation,
                retraction,
                revision,
            } => {
                revision
                    .validate()
                    .map_err(|error| Self::invalid(error.to_string()))?;
                validate_actor_run_revision(revision)?;
                if self.actor.run_key != revision.actor_run
                    || self.actor.root_session_key != revision.session
                    || self.actor.role != revision.role
                    || self.actor.parent_run_key != revision.parent_actor_run
                    || self.actor.native_session_id != revision.native_session_id
                    || self.actor.native_actor_id != revision.native_actor_id
                    || self.actor.native_actor_type != revision.native_actor_type
                {
                    return Err(Self::invalid(
                        "actor-run event context does not match its normalized revision",
                    ));
                }
                if self.native_time.is_some() || self.evidence.effective_at.is_some() {
                    return Err(Self::invalid(
                        "actor-run event cannot fabricate an effective timestamp",
                    ));
                }
                (
                    fact_family.as_str(),
                    *fact_family_contract_version,
                    *fact_id,
                    *operation,
                    retraction.as_ref().map(ActorRetractionWire::to_internal),
                    revision.session,
                    revision.actor_run,
                    None,
                    revision
                        .semantic_revision_key()
                        .map_err(|error| Self::invalid(error.to_string()))?,
                )
            }
            ActorEventWire::ActorAffiliation {
                fact_family,
                fact_family_contract_version,
                fact_id,
                operation,
                retraction,
                revision,
            } => {
                revision
                    .validate()
                    .map_err(|error| Self::invalid(error.to_string()))?;
                validate_affiliation_revision(revision)?;
                if self.native_time != revision.effective_at
                    || self.evidence.effective_at != revision.effective_at
                {
                    return Err(Self::invalid(
                        "actor-affiliation event time does not match its normalized revision",
                    ));
                }
                if !self
                    .affiliations
                    .derived_from_revision_refs
                    .contains(&self.semantic_revision_ref)
                {
                    return Err(Self::invalid(
                        "actor-affiliation context does not consume its own revision",
                    ));
                }
                (
                    fact_family.as_str(),
                    *fact_family_contract_version,
                    *fact_id,
                    *operation,
                    retraction.as_ref().map(ActorRetractionWire::to_internal),
                    revision.session,
                    revision.actor_run,
                    revision.effective_at.clone(),
                    revision
                        .semantic_revision_key()
                        .map_err(|error| Self::invalid(error.to_string()))?,
                )
            }
        };
        let expected_family_version = match family {
            ACTOR_RUN_FAMILY => RUNTIME_ACTOR_RUN_FACT_FAMILY_CONTRACT_VERSION,
            ACTOR_AFFILIATION_FAMILY => RUNTIME_ACTOR_AFFILIATION_FACT_FAMILY_CONTRACT_VERSION,
            _ => return Err(Self::invalid("actor event has an unsupported fact family")),
        };
        if family_version != expected_family_version
            || self
                .contract_selection
                .contract_versions
                .fact_family_versions
                .get(family)
                != Some(&expected_family_version)
            || self
                .semantic_revision_ref
                .semantic_reference_contract_version
                != self
                    .contract_selection
                    .contract_versions
                    .semantic_revision_reference_version
        {
            return Err(Self::invalid(
                "actor event does not match its selected family contract",
            ));
        }
        if session != self.root.session_key || actor_run != self.actor.run_key {
            return Err(Self::invalid(
                "actor event does not belong to the exact root and actor",
            ));
        }
        validate_nonzero(fact_id.as_bytes(), "actor fact id")?;
        match (operation, retraction) {
            (ActorOperationWire::Upsert, None) => {
                if self.evidence.authority != ActorEvidenceAuthorityWire::NativeRecord
                    || self.evidence.quality != QualifiedValueQuality::Exact
                {
                    return Err(Self::invalid("actor upsert has invalid evidence"));
                }
            }
            (ActorOperationWire::Retract, Some(cause)) => {
                if self.evidence.authority != ActorEvidenceAuthorityWire::CommonReducer
                    || self.evidence.quality != QualifiedValueQuality::Derived
                {
                    return Err(Self::invalid("actor retraction has invalid evidence"));
                }
                validate_retraction(cause, self.source.generation)?;
            }
            _ => {
                return Err(Self::invalid(
                    "actor operation and retraction cause are inconsistent",
                ))
            }
        }
        if self.evidence.completeness != ContractCompleteness::Complete
            || self.evidence.effective_at != effective_at
        {
            return Err(Self::invalid(
                "actor event evidence is not complete for the normalized revision",
            ));
        }
        if self.native_evidence.kind != "withheld"
            || self.native_evidence.state != SourceRecordStateWire::Present
            || self.native_evidence.reason != "projection_boundary"
        {
            return Err(Self::invalid(
                "unsupported actor native evidence projection",
            ));
        }
        validate_media_type(&self.native_evidence.media_type)?;
        let payload_hash = decode_opaque_exact(
            &self.native_evidence.payload_hash,
            "native payload hash",
            DIGEST_BYTES,
        )?;
        validate_nonzero(&payload_hash, "native payload hash")?;
        let expected_revision = FactRevisionId::derive(&fact_id, 1, &revision_key)
            .map_err(|error| Self::invalid(error.to_string()))?;
        if self.semantic_revision_ref.fact_revision_id != expected_revision {
            return Err(Self::invalid(
                "semantic revision reference does not match the actor value",
            ));
        }
        let semantic = FactSemanticRevision {
            source_record_id: self.source.source_record_id,
            fact_id,
            fact_revision_id: expected_revision,
            semantic_revision_ref: self.semantic_revision_ref,
        };
        let expected_event_id =
            revisioned_entity_event_id(family.as_bytes(), operation.into(), &semantic, retraction);
        if decode_opaque_exact(&self.event_id, "event_id", DIGEST_BYTES)?
            != expected_event_id.as_bytes()
        {
            return Err(Self::invalid(
                "event_id does not match the exact actor semantic event",
            ));
        }
        Ok(())
    }

    fn validate_context(
        &self,
        expected_root: &ScopedObservationRootIdentity,
        expected_sources: &[ScopedSourceObjectIdentity],
    ) -> Result<(), ScopedActorEnvelopeContractError> {
        if self.root.session_ref != expected_root.session_ref
            || self.root.session_key != expected_root.session_key
            || self.root.root_actor_run_key != expected_root.root_actor_run_key
            || self.root.native_session_claim != expected_root.native_session_claim
        {
            return Err(Self::invalid(
                "actor envelope does not match the caller-held root",
            ));
        }
        if expected_sources.is_empty() || expected_sources.len() > MAX_AUTHORIZED_SOURCES {
            return Err(Self::invalid(
                "actor source is outside the caller-held authorized source set",
            ));
        }
        let unique_sources = expected_sources.iter().collect::<BTreeSet<_>>();
        if unique_sources.len() != expected_sources.len()
            || !expected_sources.iter().any(|source| {
                source.adapter_id == expected_root.adapter_id
                    && source.source_instance_key == expected_root.source_instance_key
                    && source.source_instance_key == self.source.instance_key
                    && source.stream_key == self.source.stream_key
                    && source.object_key == self.source.object_key
            })
        {
            return Err(Self::invalid(
                "actor source is outside the caller-held authorized source set",
            ));
        }
        Ok(())
    }
}

fn validate_native_claim(root: &ActorRootWire) -> Result<(), ScopedActorEnvelopeContractError> {
    let Some(claim) = &root.native_session_claim else {
        return Ok(());
    };
    if claim.entity_ref != root.session_ref {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "native session claim retargets the root",
        ));
    }
    let unknown = claim.identity.quality == QualifiedValueQuality::Unknown;
    if unknown != claim.identity.value.is_none()
        || unknown != claim.identity.unknown_reason.is_some()
    {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "native session claim has inconsistent qualified-value state",
        ));
    }
    if let Some(identity) = &claim.identity.value {
        validate_canonical_text(
            &identity.native_namespace,
            "native identity namespace",
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_canonical_text(&identity.native_id, "native identity", MAX_IDENTIFIER_BYTES)?;
    }
    if let Some(effective_at) = claim.identity.effective_at {
        require_safe_i64(effective_at, "native session claim effective_at")?;
    }
    validate_canonical_text(
        &claim.identity.authority,
        "native session claim authority",
        MAX_IDENTIFIER_BYTES,
    )?;
    if claim.identity.provenance.len() > MAX_AFFILIATION_REVISIONS
        || claim
            .identity
            .provenance
            .windows(2)
            .any(|pair| pair[0].fact_revision_id >= pair[1].fact_revision_id)
    {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "native session claim provenance is not bounded and canonical",
        ));
    }
    Ok(())
}

fn validate_actor_context(
    actor: &ActorContextWire,
    root: &ActorRootWire,
    selection: &ObservationContractSelection,
) -> Result<(), ScopedActorEnvelopeContractError> {
    validate_nonzero(actor.run_key.as_bytes(), "actor run key")?;
    for (label, value) in [
        ("native_session_id", actor.native_session_id.as_deref()),
        ("native_actor_id", actor.native_actor_id.as_deref()),
        ("native_actor_type", actor.native_actor_type.as_deref()),
    ] {
        if let Some(value) = value {
            validate_canonical_text(value, label, MAX_RUNTIME_TEXT_BYTES)?;
        }
    }
    let actor_selected = selection
        .contract_versions
        .fact_family_versions
        .get(ACTOR_RUN_FAMILY)
        == Some(&RUNTIME_ACTOR_RUN_FACT_FAMILY_CONTRACT_VERSION);
    if actor_selected {
        match actor.role {
            ActorRunRole::Root if actor.parent_run_key.is_some() => {
                return Err(ScopedActorEnvelopeContractError::invalid(
                    "selected root actor cannot declare a parent",
                ))
            }
            ActorRunRole::Child
                if actor.parent_run_key.as_ref() == Some(&actor.run_key)
                    || (actor.parent_run_key.is_none()
                        && (actor.native_actor_id.is_some()
                            || actor.native_actor_type.is_some())) =>
            {
                return Err(ScopedActorEnvelopeContractError::invalid(
                    "selected child actor enrichment requires a distinct parent",
                ))
            }
            ActorRunRole::Root | ActorRunRole::Child => {}
        }
    } else if actor.parent_run_key.is_some()
        || actor.native_actor_id.is_some()
        || actor.native_actor_type.is_some()
    {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "actor enrichment requires selected runtime.actor-run@1",
        ));
    }
    if let Some(parent) = actor.parent_run_key {
        validate_nonzero(parent.as_bytes(), "parent actor-run key")?;
    }
    let expected_native_session = root
        .native_session_claim
        .as_ref()
        .and_then(|claim| claim.identity.value.as_ref())
        .map(|identity| identity.native_id.as_str());
    if actor.native_session_id.as_deref() != expected_native_session {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "actor native session does not match the root claim",
        ));
    }
    Ok(())
}

fn validate_affiliations(
    affiliations: &ActorAffiliationsWire,
    selection: &ObservationContractSelection,
    actor_run_key: CanonicalEntityKey,
) -> Result<(), ScopedActorEnvelopeContractError> {
    for (label, value) in [
        ("native_team_id", affiliations.native_team_id.as_deref()),
        ("team_name", affiliations.team_name.as_deref()),
        (
            "native_workflow_id",
            affiliations.native_workflow_id.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            validate_canonical_text(value, label, MAX_RUNTIME_TEXT_BYTES)?;
        }
    }
    for (label, value) in [
        ("team key", affiliations.team_key),
        ("member key", affiliations.member_key),
        ("workflow key", affiliations.workflow_key),
    ] {
        if let Some(value) = value {
            validate_nonzero(value.as_bytes(), label)?;
        }
    }
    for reference in &affiliations.derived_from_revision_refs {
        validate_nonzero(
            reference.fact_revision_id.as_bytes(),
            "affiliation revision reference",
        )?;
    }
    let affiliation_selected = selection
        .contract_versions
        .fact_family_versions
        .get(ACTOR_AFFILIATION_FAMILY)
        == Some(&RUNTIME_ACTOR_AFFILIATION_FACT_FAMILY_CONTRACT_VERSION);
    if !affiliation_selected {
        if affiliations.team_key.is_none()
            && affiliations.native_team_id.is_none()
            && affiliations.team_name.is_none()
            && affiliations.member_key.is_none()
            && affiliations.workflow_key.is_none()
            && affiliations.native_workflow_id.is_none()
            && affiliations.completeness == ContractCompleteness::Unknown
            && affiliations.derived_from_revision_refs.is_empty()
        {
            return Ok(());
        }
        return Err(ScopedActorEnvelopeContractError::invalid(
            "affiliation enrichment requires selected runtime.actor-affiliation@1",
        ));
    }
    let context = ScopedActorAffiliationContext {
        actor_run_key: affiliations.actor_run_key,
        team_key: affiliations.team_key,
        native_team_id: affiliations.native_team_id.clone(),
        team_name: affiliations.team_name.clone(),
        member_key: affiliations.member_key,
        workflow_key: affiliations.workflow_key,
        native_workflow_id: affiliations.native_workflow_id.clone(),
        completeness: affiliations.completeness,
        derived_from_revision_refs: affiliations.derived_from_revision_refs.clone(),
    };
    if context.actor_run_key != actor_run_key
        || !scoped_actor_affiliation_context_is_valid(
            &context,
            selection
                .contract_versions
                .semantic_revision_reference_version,
        )
    {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "selected actor affiliation context is invalid",
        ));
    }
    Ok(())
}

fn validate_actor_run_revision(
    revision: &ActorRunRevisionFact,
) -> Result<(), ScopedActorEnvelopeContractError> {
    validate_nonzero(revision.actor_run.as_bytes(), "actor-run revision key")?;
    validate_nonzero(revision.session.as_bytes(), "actor-run session key")?;
    if let Some(parent) = revision.parent_actor_run {
        validate_nonzero(parent.as_bytes(), "actor-run parent key")?;
    }
    Ok(())
}

fn validate_affiliation_revision(
    revision: &ActorAffiliationRevisionFact,
) -> Result<(), ScopedActorEnvelopeContractError> {
    for (label, value) in [
        ("affiliation key", revision.affiliation),
        ("affiliation actor-run key", revision.actor_run),
        ("affiliation session key", revision.session),
        ("affiliation target key", revision.target),
    ] {
        validate_nonzero(value.as_bytes(), label)?;
    }
    if let Some(member) = revision.member {
        validate_nonzero(member.as_bytes(), "affiliation member key")?;
    }
    validate_timestamp(revision.effective_at.as_ref(), "affiliation effective_at")
}

fn validate_source(source: &ActorSourceWire) -> Result<(), ScopedActorEnvelopeContractError> {
    if source.locator_id.is_some() {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "actor envelope cannot disclose a native locator",
        ));
    }
    require_positive_safe(source.generation, "source generation")?;
    validate_nonzero(source.instance_key.as_bytes(), "source instance key")?;
    validate_nonzero(source.stream_key.as_bytes(), "source stream key")?;
    validate_nonzero(source.object_key.as_bytes(), "source object key")?;
    validate_nonzero(source.source_record_id.as_bytes(), "source record id")?;
    let start_bytes = decode_opaque(&source.cursor_start, "cursor_start", MAX_CURSOR_BYTES)?;
    let end_bytes = decode_opaque(&source.cursor_end, "cursor_end", MAX_CURSOR_BYTES)?;
    let start = SourceCursor::from_opaque(start_bytes)
        .map_err(|error| ScopedActorEnvelopeContractError::invalid(error.to_string()))?
        .append_offset_value()
        .ok_or_else(|| {
            ScopedActorEnvelopeContractError::invalid("actor cursor_start is not append-bound")
        })?;
    let end = SourceCursor::from_opaque(end_bytes)
        .map_err(|error| ScopedActorEnvelopeContractError::invalid(error.to_string()))?
        .append_offset_value()
        .ok_or_else(|| {
            ScopedActorEnvelopeContractError::invalid("actor cursor_end is not append-bound")
        })?;
    require_safe_u64(start, "cursor_start offset")?;
    require_safe_u64(end, "cursor_end offset")?;
    require_safe_u64(source.byte_range.start, "byte_range start")?;
    require_safe_u64(source.byte_range.end, "byte_range end")?;
    if start > end || source.byte_range.start != start || source.byte_range.end != end {
        return Err(ScopedActorEnvelopeContractError::invalid(
            "actor byte range does not match its append cursors",
        ));
    }
    Ok(())
}

fn validate_retraction(
    cause: ScopedRevisionedEntityRetractionCause,
    source_generation: u64,
) -> Result<(), ScopedActorEnvelopeContractError> {
    match cause {
        ScopedRevisionedEntityRetractionCause::Reset(reset) => {
            require_positive_safe(reset.old_generation, "reset old generation")?;
            require_positive_safe(reset.new_generation, "reset new generation")?;
            if reset.old_generation != source_generation
                || reset.old_generation.checked_add(1) != Some(reset.new_generation)
                || matches!(
                    reset.reason,
                    AppendTransition::Initial | AppendTransition::Continued
                )
            {
                return Err(ScopedActorEnvelopeContractError::invalid(
                    "actor reset retraction has invalid generation lineage",
                ));
            }
        }
        ScopedRevisionedEntityRetractionCause::SourceDeleted { generation } => {
            require_positive_safe(generation, "deleted generation")?;
            if generation != source_generation {
                return Err(ScopedActorEnvelopeContractError::invalid(
                    "actor deletion retraction targets a foreign generation",
                ));
            }
        }
    }
    Ok(())
}

fn validate_timestamp(
    value: Option<&QualifiedTimestamp>,
    label: &str,
) -> Result<(), ScopedActorEnvelopeContractError> {
    if let Some(value) = value {
        validate_canonical_text(&value.value, label, MAX_RUNTIME_TEXT_BYTES)?;
    }
    Ok(())
}

fn validate_media_type(value: &str) -> Result<(), ScopedActorEnvelopeContractError> {
    validate_canonical_text(value, "native evidence media_type", MAX_MEDIA_TYPE_BYTES)
}

fn validate_canonical_text(
    value: &str,
    label: &str,
    max_bytes: usize,
) -> Result<(), ScopedActorEnvelopeContractError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} is not bounded canonical text"
        )));
    }
    Ok(())
}

fn validate_nonzero(value: &[u8], label: &str) -> Result<(), ScopedActorEnvelopeContractError> {
    if value.iter().all(|byte| *byte == 0) {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} must not be the zero reference"
        )));
    }
    Ok(())
}

fn encode_opaque(bytes: &[u8]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque(
    value: &str,
    label: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, ScopedActorEnvelopeContractError> {
    let encoded = value
        .strip_prefix(REFERENCE_PREFIX)
        .ok_or_else(|| ScopedActorEnvelopeContractError::invalid(format!("{label} is not v1")))?;
    if encoded.is_empty() || encoded.contains('=') {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedActorEnvelopeContractError::invalid(format!("{label} is not canonical base64url"))
    })?;
    if decoded.is_empty()
        || decoded.len() > max_bytes
        || URL_SAFE_NO_PAD.encode(&decoded) != encoded
    {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} is not bounded canonical base64url"
        )));
    }
    Ok(decoded)
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
    expected_bytes: usize,
) -> Result<Vec<u8>, ScopedActorEnvelopeContractError> {
    let decoded = decode_opaque(value, label, expected_bytes)?;
    if decoded.len() != expected_bytes {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} must contain exactly {expected_bytes} bytes"
        )));
    }
    Ok(decoded)
}

fn require_positive_safe(value: u64, label: &str) -> Result<(), ScopedActorEnvelopeContractError> {
    if value == 0 || value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} must be a positive portable safe integer"
        )));
    }
    Ok(())
}

fn require_safe_u64(value: u64, label: &str) -> Result<(), ScopedActorEnvelopeContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable safe-integer range"
        )));
    }
    Ok(())
}

fn require_safe_i64(value: i64, label: &str) -> Result<(), ScopedActorEnvelopeContractError> {
    if !(-(JS_SAFE_INTEGER_MAX as i64)..=JS_SAFE_INTEGER_MAX as i64).contains(&value) {
        return Err(ScopedActorEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable safe-integer range"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
