//! Strict portable RFC 012D wire projection for the currently implemented
//! `runtime.usage-v2` event family.
//!
//! This is deliberately narrower than the selected observation envelope/event
//! contracts: source controls, observer lifecycle controls, and future fact
//! families remain unavailable until the complete union and its typed-unknown
//! rules are frozen. The top-level value has no unbound `Deserialize` path;
//! consumption requires the caller-held selection, root, and authorized source
//! set. Usage-only selection preserves the original key-only actor context;
//! evidence-backed actor and affiliation enrichment is accepted only when its
//! exact v1 family is also selected.

use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{
    ActorRunRole, CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey,
    ContractCompleteness, CoverageObjectKey, CoverageStreamKey, ExternalEntityRef, FactRevisionId,
    FactSemanticRevision, NativeIdentityClaim, QualifiedTimestamp, QualifiedValueQuality,
    SemanticRevisionRef, SourceRecordId, UsageQualifiedValue, UsageRevisionV2Fact,
};
use crate::observation_contract::ObservationContractSelection;
use crate::source::{AppendTransition, SourceCursor, SourceRecordState};

use super::{
    scoped_actor_affiliation_context_is_valid, usage_v2_event_id, ScopedActorAffiliationContext,
    ScopedActorAttribution, ScopedAppendDeliveryPhase, ScopedAppendReset,
    ScopedEnvelopeEvidenceAuthority, ScopedNativeEvidence, ScopedNativeEvidenceWithheldReason,
    ScopedObservationEnvelope, ScopedObservationEvent, ScopedObservationRootIdentity,
    ScopedSourceObjectIdentity, ScopedUsageV2Operation, ScopedUsageV2RetractionCause,
    RUNTIME_ACTOR_AFFILIATION_FACT_FAMILY_CONTRACT_VERSION,
    RUNTIME_ACTOR_RUN_FACT_FAMILY_CONTRACT_VERSION, RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION,
};

pub(crate) const SCOPED_USAGE_ENVELOPE_CONTRACT_VERSION: u32 = 1;

const USAGE_FAMILY: &str = "runtime.usage-v2";
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
pub(crate) enum ScopedUsageEnvelopeContractError {
    #[error("invalid scoped usage envelope contract: {message}")]
    Invalid { message: String },
    #[error("scoped envelope event is not supported by the usage-v2 wire contract")]
    UnsupportedEvent,
}

impl ScopedUsageEnvelopeContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UsageDeliveryPhaseWire {
    Bootstrap,
    Live,
    Correction,
}

impl From<ScopedAppendDeliveryPhase> for UsageDeliveryPhaseWire {
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
enum UsageOperationWire {
    Upsert,
    Retract,
}

impl From<ScopedUsageV2Operation> for UsageOperationWire {
    fn from(value: ScopedUsageV2Operation) -> Self {
        match value {
            ScopedUsageV2Operation::Upsert => Self::Upsert,
            ScopedUsageV2Operation::Retract => Self::Retract,
        }
    }
}

impl From<UsageOperationWire> for ScopedUsageV2Operation {
    fn from(value: UsageOperationWire) -> Self {
        match value {
            UsageOperationWire::Upsert => Self::Upsert,
            UsageOperationWire::Retract => Self::Retract,
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
    type Error = ScopedUsageEnvelopeContractError;

    fn try_from(value: AppendTransition) -> Result<Self, Self::Error> {
        match value {
            AppendTransition::Truncated => Ok(Self::Truncated),
            AppendTransition::IdentityChanged => Ok(Self::IdentityChanged),
            AppendTransition::PrefixMismatch => Ok(Self::PrefixMismatch),
            AppendTransition::ContractReplay => Ok(Self::ContractReplay),
            AppendTransition::Initial | AppendTransition::Continued => {
                Err(ScopedUsageEnvelopeContractError::invalid(
                    "usage reset retraction requires a reset transition",
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
enum UsageRetractionWire {
    Reset {
        old_generation: u64,
        new_generation: u64,
        reason: AppendTransitionWire,
    },
    SourceDeleted {
        generation: u64,
    },
}

impl UsageRetractionWire {
    fn from_internal(
        value: ScopedUsageV2RetractionCause,
    ) -> Result<Self, ScopedUsageEnvelopeContractError> {
        match value {
            ScopedUsageV2RetractionCause::Reset(reset) => Ok(Self::Reset {
                old_generation: reset.old_generation,
                new_generation: reset.new_generation,
                reason: reset.reason.try_into()?,
            }),
            ScopedUsageV2RetractionCause::SourceDeleted { generation } => {
                Ok(Self::SourceDeleted { generation })
            }
        }
    }

    fn to_internal(&self) -> ScopedUsageV2RetractionCause {
        match *self {
            Self::Reset {
                old_generation,
                new_generation,
                reason,
            } => ScopedUsageV2RetractionCause::Reset(ScopedAppendReset {
                old_generation,
                new_generation,
                reason: reason.into(),
            }),
            Self::SourceDeleted { generation } => {
                ScopedUsageV2RetractionCause::SourceDeleted { generation }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageRootWire {
    session_ref: ExternalEntityRef,
    session_key: CanonicalEntityKey,
    root_actor_run_key: CanonicalEntityKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_claim: Option<NativeIdentityClaim>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageActorWire {
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
enum UsageActorAttributionWire {
    DerivedExact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageAffiliationsWire {
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
struct UsageByteRangeWire {
    start: u64,
    end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageSourceWire {
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
    byte_range: UsageByteRangeWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UsageEvidenceAuthorityWire {
    NativeRecord,
    CommonReducer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageEvidenceWire {
    authority: UsageEvidenceAuthorityWire,
    quality: QualifiedValueQuality,
    #[serde(deserialize_with = "deserialize_required_option")]
    effective_at: Option<QualifiedTimestamp>,
    completeness: ContractCompleteness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageEventWire {
    kind: String,
    fact_family: String,
    fact_family_contract_version: u32,
    fact_id: CanonicalFactId,
    operation: UsageOperationWire,
    #[serde(deserialize_with = "deserialize_required_option")]
    retraction: Option<UsageRetractionWire>,
    #[serde(deserialize_with = "deserialize_required_usage_revision")]
    revision: UsageRevisionV2Fact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceRecordStateWire {
    Present,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageNativeEvidenceWire {
    kind: String,
    media_type: String,
    state: SourceRecordStateWire,
    payload_hash: String,
    reason: String,
}

/// Specialized, serialization-only v1 projection of one mapped usage event.
/// It does not implement `Deserialize`; callers must use
/// `from_wire_value_for_context` and provide the retained negotiation/root/
/// source authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedUsageEnvelopeWire {
    scoped_usage_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: ObservationContractSelection,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: SemanticRevisionRef,
    root: UsageRootWire,
    actor: UsageActorWire,
    actor_attribution: UsageActorAttributionWire,
    affiliations: UsageAffiliationsWire,
    source: UsageSourceWire,
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: UsageDeliveryPhaseWire,
    evidence: UsageEvidenceWire,
    event: UsageEventWire,
    native_evidence: UsageNativeEvidenceWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedUsageEnvelopeInput {
    scoped_usage_envelope_contract_version: u32,
    contract_version: u32,
    contract_selection: JsonValue,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: SemanticRevisionRef,
    root: UsageRootWire,
    actor: UsageActorWire,
    actor_attribution: UsageActorAttributionWire,
    affiliations: UsageAffiliationsWire,
    source: UsageSourceWire,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_time: Option<QualifiedTimestamp>,
    observed_at: i64,
    phase: UsageDeliveryPhaseWire,
    evidence: UsageEvidenceWire,
    event: UsageEventWire,
    native_evidence: UsageNativeEvidenceWire,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn deserialize_required_usage_revision<'de, D>(
    deserializer: D,
) -> Result<UsageRevisionV2Fact, D::Error>
where
    D: Deserializer<'de>,
{
    let value = JsonValue::deserialize(deserializer)?;
    let object = value
        .as_object()
        .ok_or_else(|| D::Error::custom("usage revision must be an object"))?;
    for field in [
        "session",
        "actor_run",
        "response_key",
        "response_identity",
        "native_message_id",
        "request_id",
        "buckets",
        "model",
        "effort",
        "source_time",
    ] {
        if !object.contains_key(field) {
            return Err(D::Error::custom(format!(
                "usage revision is missing field {field}"
            )));
        }
    }
    serde_json::from_value(value).map_err(D::Error::custom)
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedUsageEnvelopeContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedUsageEnvelopeContractError::invalid(format!("{label} must be an object"))
    })?;
    for field in required {
        if !object.contains_key(*field) {
            return Err(ScopedUsageEnvelopeContractError::invalid(format!(
                "{label} is missing field {field}"
            )));
        }
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} contains unknown field {field}"
        )));
    }
    Ok(object)
}

fn validate_semantic_ref_shape(
    value: &JsonValue,
    label: &str,
) -> Result<(), ScopedUsageEnvelopeContractError> {
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
) -> Result<(), ScopedUsageEnvelopeContractError> {
    if !value.is_null() {
        exact_object(value, label, &["value", "quality"], &[])?;
    }
    Ok(())
}

fn validate_native_claim_shape(value: &JsonValue) -> Result<(), ScopedUsageEnvelopeContractError> {
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
        ScopedUsageEnvelopeContractError::invalid(
            "native session claim provenance must be an array",
        )
    })?;
    if provenance.len() > MAX_AFFILIATION_REVISIONS {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "native session claim provenance exceeds its bound",
        ));
    }
    for reference in provenance {
        validate_semantic_ref_shape(reference, "native session claim provenance")?;
    }
    Ok(())
}

fn validate_usage_qualified_shape(
    value: &JsonValue,
    label: &str,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    let qualified = exact_object(
        value,
        label,
        &[
            "value",
            "quality",
            "authority",
            "completeness",
            "provenance",
        ],
        &["unknown_reason", "effective_at"],
    )?;
    exact_object(
        &qualified["provenance"],
        "usage normalization provenance",
        &["native_field", "normalization_contract_version"],
        &[],
    )?;
    Ok(())
}

fn validate_usage_revision_shape(
    value: &JsonValue,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    let revision = exact_object(
        value,
        "usage revision",
        &[
            "session",
            "actor_run",
            "response_key",
            "response_identity",
            "native_message_id",
            "request_id",
            "buckets",
            "model",
            "effort",
            "source_time",
        ],
        &[],
    )?;
    let buckets = exact_object(
        &revision["buckets"],
        "usage buckets",
        &[
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ],
        &[],
    )?;
    for field in [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ] {
        validate_usage_qualified_shape(&buckets[field], field)?;
    }
    for field in ["model", "effort"] {
        if !revision[field].is_null() {
            validate_usage_qualified_shape(&revision[field], field)?;
        }
    }
    validate_timestamp_shape(&revision["source_time"], "usage source_time")
}

fn validate_usage_envelope_raw_shape(
    value: &JsonValue,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    let envelope = exact_object(
        value,
        "scoped usage envelope",
        &[
            "scoped_usage_envelope_contract_version",
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
        "usage root",
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
        "usage actor",
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
        "usage affiliations",
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
            ScopedUsageEnvelopeContractError::invalid(
                "usage affiliation revision refs must be an array",
            )
        })?;
    if affiliation_refs.len() > MAX_AFFILIATION_REVISIONS {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "usage affiliation revision refs exceed their bound",
        ));
    }
    for reference in affiliation_refs {
        validate_semantic_ref_shape(reference, "usage affiliation revision ref")?;
    }
    let source = exact_object(
        &envelope["source"],
        "usage source",
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
        "usage byte range",
        &["start", "end"],
        &[],
    )?;
    validate_timestamp_shape(&envelope["native_time"], "usage native time")?;
    let evidence = exact_object(
        &envelope["evidence"],
        "usage evidence",
        &["authority", "quality", "effective_at", "completeness"],
        &[],
    )?;
    validate_timestamp_shape(&evidence["effective_at"], "usage evidence effective_at")?;
    let event = exact_object(
        &envelope["event"],
        "usage event",
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
    if !event["retraction"].is_null() {
        let retraction = event["retraction"].as_object().ok_or_else(|| {
            ScopedUsageEnvelopeContractError::invalid("usage retraction must be an object")
        })?;
        match retraction.get("kind").and_then(JsonValue::as_str) {
            Some("reset") => {
                exact_object(
                    &event["retraction"],
                    "usage reset retraction",
                    &["kind", "old_generation", "new_generation", "reason"],
                    &[],
                )?;
            }
            Some("source_deleted") => {
                exact_object(
                    &event["retraction"],
                    "usage deletion retraction",
                    &["kind", "generation"],
                    &[],
                )?;
            }
            _ => {
                return Err(ScopedUsageEnvelopeContractError::invalid(
                    "usage retraction has an unsupported kind",
                ));
            }
        }
    }
    validate_usage_revision_shape(&event["revision"])?;
    exact_object(
        &envelope["native_evidence"],
        "usage native evidence",
        &["kind", "media_type", "state", "payload_hash", "reason"],
        &[],
    )?;
    Ok(())
}

impl ScopedUsageEnvelopeWire {
    pub(crate) fn from_scoped(
        envelope: &ScopedObservationEnvelope,
    ) -> Result<Self, ScopedUsageEnvelopeContractError> {
        let ScopedObservationEvent::UsageV2 {
            fact_id,
            operation,
            retraction,
            revision,
        } = &envelope.event
        else {
            return Err(ScopedUsageEnvelopeContractError::UnsupportedEvent);
        };
        let ScopedNativeEvidence::Withheld {
            media_type,
            state,
            payload_hash,
            reason,
        } = &envelope.native_evidence
        else {
            return Err(ScopedUsageEnvelopeContractError::UnsupportedEvent);
        };
        if *state != SourceRecordState::Present
            || *reason != ScopedNativeEvidenceWithheldReason::ProjectionBoundary
        {
            return Err(ScopedUsageEnvelopeContractError::UnsupportedEvent);
        }
        let source_record_id = envelope
            .source
            .source_record_id
            .ok_or_else(|| Self::invalid("usage source is missing source_record_id"))?;
        let record_index = envelope
            .source
            .record_index
            .ok_or_else(|| Self::invalid("usage source is missing record_index"))?;
        let cursor_start = envelope
            .source
            .cursor_start
            .as_ref()
            .ok_or_else(|| Self::invalid("usage source is missing cursor_start"))?;
        let cursor_end = envelope
            .source
            .cursor_end
            .as_ref()
            .ok_or_else(|| Self::invalid("usage source is missing cursor_end"))?;
        let byte_range = envelope
            .source
            .byte_range
            .ok_or_else(|| Self::invalid("usage source is missing byte_range"))?;
        let semantic_revision_ref = envelope
            .semantic_revision_ref
            .ok_or_else(|| Self::invalid("usage envelope is missing semantic_revision_ref"))?;
        let authority = match envelope.evidence.authority {
            ScopedEnvelopeEvidenceAuthority::NativeRecord => {
                UsageEvidenceAuthorityWire::NativeRecord
            }
            ScopedEnvelopeEvidenceAuthority::CommonReducer => {
                UsageEvidenceAuthorityWire::CommonReducer
            }
            ScopedEnvelopeEvidenceAuthority::EngineControl
            | ScopedEnvelopeEvidenceAuthority::PreservedUnknownWire => {
                return Err(ScopedUsageEnvelopeContractError::UnsupportedEvent)
            }
        };
        if envelope.actor_attribution != ScopedActorAttribution::DerivedExact {
            return Err(ScopedUsageEnvelopeContractError::UnsupportedEvent);
        }
        let value = Self {
            scoped_usage_envelope_contract_version: SCOPED_USAGE_ENVELOPE_CONTRACT_VERSION,
            contract_version: envelope.contract_version,
            contract_selection: envelope.contract_selection.clone(),
            observer_sequence: envelope.observer_sequence,
            scope_epoch: envelope.scope_epoch,
            event_id: encode_opaque(envelope.event_id.as_bytes()),
            semantic_revision_ref,
            root: UsageRootWire {
                session_ref: envelope.root.session_ref,
                session_key: envelope.root.session_key,
                root_actor_run_key: envelope.root.root_actor_run_key,
                native_session_claim: envelope.root.native_session_claim.clone(),
            },
            actor: UsageActorWire {
                root_session_key: envelope.actor.root_session_key,
                run_key: envelope.actor.run_key,
                role: envelope.actor.role,
                parent_run_key: envelope.actor.parent_run_key,
                native_session_id: envelope.actor.native_session_id.clone(),
                native_actor_id: envelope.actor.native_actor_id.clone(),
                native_actor_type: envelope.actor.native_actor_type.clone(),
            },
            actor_attribution: UsageActorAttributionWire::DerivedExact,
            affiliations: UsageAffiliationsWire {
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
            source: UsageSourceWire {
                instance_key: envelope.source.instance_key,
                stream_key: envelope.source.stream_key,
                object_key: envelope.source.object_key,
                locator_id: envelope.source.locator_id.clone(),
                generation: envelope.source.generation,
                source_record_id,
                record_index,
                cursor_start: encode_opaque(cursor_start.as_bytes()),
                cursor_end: encode_opaque(cursor_end.as_bytes()),
                byte_range: UsageByteRangeWire {
                    start: byte_range.start,
                    end: byte_range.end,
                },
            },
            native_time: envelope.native_time.clone(),
            observed_at: envelope.observed_at,
            phase: envelope.phase.into(),
            evidence: UsageEvidenceWire {
                authority,
                quality: envelope.evidence.quality,
                effective_at: envelope.evidence.effective_at.clone(),
                completeness: envelope.evidence.completeness,
            },
            event: UsageEventWire {
                kind: "usage_v2".to_owned(),
                fact_family: USAGE_FAMILY.to_owned(),
                fact_family_contract_version: RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION,
                fact_id: *fact_id,
                operation: (*operation).into(),
                retraction: retraction
                    .map(UsageRetractionWire::from_internal)
                    .transpose()?,
                revision: (**revision).clone(),
            },
            native_evidence: UsageNativeEvidenceWire {
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

    /// Strict contextual consumption. Exact selection/root/source authority is
    /// caller-held; it cannot be learned from the received envelope itself.
    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        expected_selection: &ObservationContractSelection,
        expected_root: &ScopedObservationRootIdentity,
        expected_sources: &[ScopedSourceObjectIdentity],
    ) -> Result<Self, ScopedUsageEnvelopeContractError> {
        validate_usage_envelope_raw_shape(&value)?;
        let input: ScopedUsageEnvelopeInput =
            serde_json::from_value(value).map_err(|error| Self::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            expected_selection,
        )
        .map_err(|error| Self::invalid(error.to_string()))?;
        let value = Self {
            scoped_usage_envelope_contract_version: input.scoped_usage_envelope_contract_version,
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

    fn invalid(message: impl Into<String>) -> ScopedUsageEnvelopeContractError {
        ScopedUsageEnvelopeContractError::invalid(message)
    }

    fn validate_common(&self) -> Result<(), ScopedUsageEnvelopeContractError> {
        if self.scoped_usage_envelope_contract_version != SCOPED_USAGE_ENVELOPE_CONTRACT_VERSION
            || self.contract_version != self.contract_selection.envelope_contract_version
            || self
                .contract_selection
                .contract_versions
                .fact_family_versions
                .get(USAGE_FAMILY)
                != Some(&RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION)
        {
            return Err(Self::invalid(
                "usage envelope does not match the selected v1 usage contract",
            ));
        }
        require_positive_safe(self.observer_sequence, "observer_sequence")?;
        require_positive_safe(self.scope_epoch, "scope_epoch")?;
        require_safe_i64(self.observed_at, "observed_at")?;
        if self.root.session_ref.entity_key != self.root.session_key
            || self.actor.root_session_key != self.root.session_key
            || self.actor.run_key != self.event.revision.actor_run
            || self.event.revision.session != self.root.session_key
            || self.affiliations.actor_run_key != self.actor.run_key
        {
            return Err(Self::invalid(
                "usage root, actor, affiliation, and revision identities disagree",
            ));
        }
        let expected_role = if self.actor.run_key == self.root.root_actor_run_key {
            ActorRunRole::Root
        } else {
            ActorRunRole::Child
        };
        if self.actor.role != expected_role {
            return Err(Self::invalid(
                "usage actor role does not match the envelope root actor",
            ));
        }
        validate_native_claim(&self.root)?;
        validate_actor(&self.actor, &self.root, &self.contract_selection)?;
        validate_affiliations(
            &self.affiliations,
            &self.contract_selection,
            self.actor.run_key,
        )?;
        validate_source(&self.source)?;
        validate_timestamp(self.native_time.as_ref(), "native_time")?;
        validate_timestamp(self.evidence.effective_at.as_ref(), "evidence effective_at")?;
        if self.native_time != self.event.revision.source_time
            || self.evidence.effective_at != self.native_time
            || self.evidence.completeness != ContractCompleteness::Complete
        {
            return Err(Self::invalid(
                "usage time/evidence does not match the normalized revision",
            ));
        }
        if self.event.kind != "usage_v2"
            || self.event.fact_family != USAGE_FAMILY
            || self.event.fact_family_contract_version
                != RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION
        {
            return Err(Self::invalid("unsupported usage event discriminator"));
        }
        validate_usage_revision(&self.event.revision)?;
        let operation: ScopedUsageV2Operation = self.event.operation.into();
        let retraction = self
            .event
            .retraction
            .as_ref()
            .map(UsageRetractionWire::to_internal);
        match (operation, retraction) {
            (ScopedUsageV2Operation::Upsert, None) => {
                if self.evidence.authority != UsageEvidenceAuthorityWire::NativeRecord
                    || self.evidence.quality != QualifiedValueQuality::Exact
                {
                    return Err(Self::invalid("usage upsert has invalid evidence"));
                }
            }
            (ScopedUsageV2Operation::Retract, Some(cause)) => {
                if self.evidence.authority != UsageEvidenceAuthorityWire::CommonReducer
                    || self.evidence.quality != QualifiedValueQuality::Derived
                {
                    return Err(Self::invalid("usage retraction has invalid evidence"));
                }
                validate_retraction(cause, self.source.generation)?;
            }
            _ => {
                return Err(Self::invalid(
                    "usage operation and retraction cause are inconsistent",
                ))
            }
        }
        if self.native_evidence.kind != "withheld"
            || self.native_evidence.state != SourceRecordStateWire::Present
            || self.native_evidence.reason != "projection_boundary"
        {
            return Err(Self::invalid("unsupported native evidence projection"));
        }
        validate_media_type(&self.native_evidence.media_type)?;
        decode_opaque_exact(
            &self.native_evidence.payload_hash,
            "native payload hash",
            DIGEST_BYTES,
        )?;
        let revision_key = self
            .event
            .revision
            .semantic_revision_key()
            .map_err(|error| Self::invalid(error.to_string()))?;
        let expected_revision = FactRevisionId::derive(&self.event.fact_id, 1, &revision_key)
            .map_err(|error| Self::invalid(error.to_string()))?;
        if self.semantic_revision_ref.fact_revision_id != expected_revision {
            return Err(Self::invalid(
                "semantic revision reference does not match the usage value",
            ));
        }
        let semantic = FactSemanticRevision {
            source_record_id: self.source.source_record_id,
            fact_id: self.event.fact_id,
            fact_revision_id: expected_revision,
            semantic_revision_ref: self.semantic_revision_ref,
        };
        let expected_event_id = usage_v2_event_id(operation, &semantic, retraction);
        if decode_opaque_exact(&self.event_id, "event_id", DIGEST_BYTES)?
            != expected_event_id.as_bytes()
        {
            return Err(Self::invalid(
                "event_id does not match the exact semantic event",
            ));
        }
        Ok(())
    }

    fn validate_context(
        &self,
        expected_root: &ScopedObservationRootIdentity,
        expected_sources: &[ScopedSourceObjectIdentity],
    ) -> Result<(), ScopedUsageEnvelopeContractError> {
        if self.root.session_ref != expected_root.session_ref
            || self.root.session_key != expected_root.session_key
            || self.root.root_actor_run_key != expected_root.root_actor_run_key
            || self.root.native_session_claim != expected_root.native_session_claim
        {
            return Err(Self::invalid(
                "usage envelope does not match the caller-held root",
            ));
        }
        if expected_sources.is_empty() || expected_sources.len() > MAX_AUTHORIZED_SOURCES {
            return Err(Self::invalid(
                "usage source is outside the caller-held authorized source set",
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
                "usage source is outside the caller-held authorized source set",
            ));
        }
        Ok(())
    }
}

fn encode_opaque(bytes: &[u8]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque(
    value: &str,
    label: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, ScopedUsageEnvelopeContractError> {
    let encoded = value
        .strip_prefix(REFERENCE_PREFIX)
        .ok_or_else(|| ScopedUsageEnvelopeContractError::invalid(format!("{label} is not v1")))?;
    if encoded.is_empty() || encoded.contains('=') {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedUsageEnvelopeContractError::invalid(format!("{label} is not canonical base64url"))
    })?;
    if decoded.is_empty()
        || decoded.len() > max_bytes
        || URL_SAFE_NO_PAD.encode(&decoded) != encoded
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} has invalid bounds or encoding"
        )));
    }
    Ok(decoded)
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
    bytes: usize,
) -> Result<Vec<u8>, ScopedUsageEnvelopeContractError> {
    let decoded = decode_opaque(value, label, bytes)?;
    if decoded.len() != bytes {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} must contain exactly {bytes} bytes"
        )));
    }
    Ok(decoded)
}

fn require_positive_safe(value: u64, label: &str) -> Result<(), ScopedUsageEnvelopeContractError> {
    if value == 0 || value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} must be a positive portable integer"
        )));
    }
    Ok(())
}

fn require_safe_u64(value: u64, label: &str) -> Result<(), ScopedUsageEnvelopeContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable integer range"
        )));
    }
    Ok(())
}

fn require_safe_i64(value: i64, label: &str) -> Result<(), ScopedUsageEnvelopeContractError> {
    let max = JS_SAFE_INTEGER_MAX as i64;
    if !(-max..=max).contains(&value) {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} exceeds the portable integer range"
        )));
    }
    Ok(())
}

fn validate_native_claim(root: &UsageRootWire) -> Result<(), ScopedUsageEnvelopeContractError> {
    let Some(claim) = &root.native_session_claim else {
        return Ok(());
    };
    if claim.entity_ref != root.session_ref {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "native session claim retargets the root",
        ));
    }
    let unknown = claim.identity.quality == QualifiedValueQuality::Unknown;
    if unknown != claim.identity.value.is_none()
        || unknown != claim.identity.unknown_reason.is_some()
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "native session claim has inconsistent qualified-value state",
        ));
    }
    if let Some(identity) = &claim.identity.value {
        for (label, value) in [
            (
                "native identity namespace",
                identity.native_namespace.as_str(),
            ),
            ("native identity", identity.native_id.as_str()),
        ] {
            if value.is_empty() || value.trim() != value || value.len() > MAX_IDENTIFIER_BYTES {
                return Err(ScopedUsageEnvelopeContractError::invalid(format!(
                    "{label} is not bounded canonical text"
                )));
            }
        }
    }
    if let Some(effective_at) = claim.identity.effective_at {
        require_safe_i64(effective_at, "native session claim effective_at")?;
    }
    if claim.identity.provenance.len() > MAX_AFFILIATION_REVISIONS
        || claim.identity.authority.is_empty()
        || claim.identity.authority.trim() != claim.identity.authority
        || claim.identity.authority.len() > MAX_IDENTIFIER_BYTES
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "native session claim has invalid bounded evidence",
        ));
    }
    if claim
        .identity
        .provenance
        .windows(2)
        .any(|pair| pair[0].fact_revision_id >= pair[1].fact_revision_id)
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "native session claim provenance is not canonical",
        ));
    }
    Ok(())
}

fn validate_actor(
    actor: &UsageActorWire,
    root: &UsageRootWire,
    selection: &ObservationContractSelection,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    for (label, value) in [
        ("native_session_id", actor.native_session_id.as_deref()),
        ("native_actor_id", actor.native_actor_id.as_deref()),
        ("native_actor_type", actor.native_actor_type.as_deref()),
    ] {
        if let Some(value) = value {
            if value.is_empty() || value.trim() != value || value.len() > MAX_RUNTIME_TEXT_BYTES {
                return Err(ScopedUsageEnvelopeContractError::invalid(format!(
                    "{label} is not bounded canonical text"
                )));
            }
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
                return Err(ScopedUsageEnvelopeContractError::invalid(
                    "selected root actor cannot declare a parent",
                ))
            }
            ActorRunRole::Child
                if actor.parent_run_key.as_ref() == Some(&actor.run_key)
                    || (actor.parent_run_key.is_none()
                        && (actor.native_actor_id.is_some()
                            || actor.native_actor_type.is_some())) =>
            {
                return Err(ScopedUsageEnvelopeContractError::invalid(
                    "selected child actor enrichment requires a distinct parent",
                ))
            }
            ActorRunRole::Root | ActorRunRole::Child => {}
        }
    } else if actor.parent_run_key.is_some()
        || actor.native_actor_id.is_some()
        || actor.native_actor_type.is_some()
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "usage actor enrichment requires selected runtime.actor-run@1",
        ));
    }
    let expected_native_session = root
        .native_session_claim
        .as_ref()
        .and_then(|claim| claim.identity.value.as_ref())
        .map(|identity| identity.native_id.as_str());
    if actor.native_session_id.as_deref() != expected_native_session {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "usage actor native session does not match the root claim",
        ));
    }
    Ok(())
}

fn validate_affiliations(
    affiliations: &UsageAffiliationsWire,
    selection: &ObservationContractSelection,
    actor_run_key: CanonicalEntityKey,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    for (label, value) in [
        ("native_team_id", affiliations.native_team_id.as_deref()),
        ("team_name", affiliations.team_name.as_deref()),
        (
            "native_workflow_id",
            affiliations.native_workflow_id.as_deref(),
        ),
    ] {
        if value.is_some_and(|value| {
            value.is_empty() || value.trim() != value || value.len() > MAX_RUNTIME_TEXT_BYTES
        }) {
            return Err(ScopedUsageEnvelopeContractError::invalid(format!(
                "{label} is not bounded canonical text"
            )));
        }
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
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "usage affiliation enrichment requires selected runtime.actor-affiliation@1",
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
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "selected usage affiliation context is invalid",
        ));
    }
    Ok(())
}

fn validate_source(source: &UsageSourceWire) -> Result<(), ScopedUsageEnvelopeContractError> {
    if source.locator_id.is_some() {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "usage envelope cannot disclose a native locator",
        ));
    }
    require_positive_safe(source.generation, "source generation")?;
    let start_bytes = decode_opaque(&source.cursor_start, "cursor_start", MAX_CURSOR_BYTES)?;
    let end_bytes = decode_opaque(&source.cursor_end, "cursor_end", MAX_CURSOR_BYTES)?;
    let start = SourceCursor::from_opaque(start_bytes)
        .map_err(|error| ScopedUsageEnvelopeContractError::invalid(error.to_string()))?
        .append_offset_value()
        .ok_or_else(|| {
            ScopedUsageEnvelopeContractError::invalid("usage cursor_start is not append-bound")
        })?;
    let end = SourceCursor::from_opaque(end_bytes)
        .map_err(|error| ScopedUsageEnvelopeContractError::invalid(error.to_string()))?
        .append_offset_value()
        .ok_or_else(|| {
            ScopedUsageEnvelopeContractError::invalid("usage cursor_end is not append-bound")
        })?;
    require_safe_u64(start, "cursor_start offset")?;
    require_safe_u64(end, "cursor_end offset")?;
    require_safe_u64(source.byte_range.start, "byte_range start")?;
    require_safe_u64(source.byte_range.end, "byte_range end")?;
    if start > end || source.byte_range.start != start || source.byte_range.end != end {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "usage byte range does not match its append cursors",
        ));
    }
    Ok(())
}

fn validate_timestamp(
    value: Option<&QualifiedTimestamp>,
    label: &str,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    if value.is_some_and(|value| {
        value.value.is_empty()
            || value.value.len() > MAX_RUNTIME_TEXT_BYTES
            || value.value.trim() != value.value
    }) {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} is not bounded canonical text"
        )));
    }
    Ok(())
}

fn validate_media_type(value: &str) -> Result<(), ScopedUsageEnvelopeContractError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_MEDIA_TYPE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(
            "native evidence media_type is not bounded canonical text",
        ));
    }
    Ok(())
}

fn validate_usage_revision(
    revision: &UsageRevisionV2Fact,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    revision
        .validate()
        .map_err(|error| ScopedUsageEnvelopeContractError::invalid(error.to_string()))?;
    for (label, value) in [
        ("native_message_id", revision.native_message_id.as_deref()),
        ("request_id", revision.request_id.as_deref()),
    ] {
        if value.is_some_and(|value| {
            value.is_empty() || value.trim() != value || value.len() > MAX_RUNTIME_TEXT_BYTES
        }) {
            return Err(ScopedUsageEnvelopeContractError::invalid(format!(
                "{label} is not bounded canonical text"
            )));
        }
    }
    for (label, value) in [
        ("input_tokens", &revision.buckets.input_tokens),
        ("output_tokens", &revision.buckets.output_tokens),
        (
            "cache_creation_input_tokens",
            &revision.buckets.cache_creation_input_tokens,
        ),
        (
            "cache_read_input_tokens",
            &revision.buckets.cache_read_input_tokens,
        ),
    ] {
        validate_usage_qualified(value, label)?;
        if let Some(value) = value.value {
            require_safe_u64(value, label)?;
        }
    }
    for (label, value) in [("model", &revision.model), ("effort", &revision.effort)] {
        if let Some(value) = value {
            validate_usage_qualified(value, label)?;
            if value.value.as_ref().is_some_and(|value| {
                value.is_empty() || value.trim() != value || value.len() > MAX_RUNTIME_TEXT_BYTES
            }) {
                return Err(ScopedUsageEnvelopeContractError::invalid(format!(
                    "{label} is not bounded canonical text"
                )));
            }
        }
    }
    validate_timestamp(revision.source_time.as_ref(), "usage source_time")
}

fn validate_usage_qualified<T>(
    value: &UsageQualifiedValue<T>,
    label: &str,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    if value.provenance.normalization_contract_version != 1
        || value.provenance.native_field.is_empty()
        || value.provenance.native_field.len() > MAX_IDENTIFIER_BYTES
        || value.provenance.native_field.trim() != value.provenance.native_field
    {
        return Err(ScopedUsageEnvelopeContractError::invalid(format!(
            "{label} has invalid normalization provenance"
        )));
    }
    if let Some(effective_at) = value.effective_at {
        require_safe_i64(effective_at, &format!("{label} effective_at"))?;
    }
    Ok(())
}

fn validate_retraction(
    cause: ScopedUsageV2RetractionCause,
    source_generation: u64,
) -> Result<(), ScopedUsageEnvelopeContractError> {
    match cause {
        ScopedUsageV2RetractionCause::Reset(reset) => {
            require_positive_safe(reset.old_generation, "reset old_generation")?;
            require_positive_safe(reset.new_generation, "reset new_generation")?;
            if reset.old_generation != source_generation
                || reset.old_generation.checked_add(1) != Some(reset.new_generation)
            {
                return Err(ScopedUsageEnvelopeContractError::invalid(
                    "reset retraction has invalid generation lineage",
                ));
            }
        }
        ScopedUsageV2RetractionCause::SourceDeleted { generation } => {
            require_positive_safe(generation, "deleted generation")?;
            if generation != source_generation {
                return Err(ScopedUsageEnvelopeContractError::invalid(
                    "source-deleted retraction targets a foreign generation",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
