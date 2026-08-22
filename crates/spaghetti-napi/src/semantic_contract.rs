//! RFC 012A/012C committed semantic fixture JSON.
//!
//! This crate-private parser is the store-free authority for the already-landed
//! coverage, actor-run, actor-affiliation, and usage-v2 value contracts. It
//! does not open a source, query, or delivery path.

use std::collections::BTreeSet;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::adapter::{
    compare_coverage, ActorAffiliationDimension, ActorAffiliationRevisionFact,
    ActorAffiliationState, ActorRunRevisionFact, ActorRunRole, AdapterError, CanonicalEntityKey,
    CanonicalFactId, CanonicalSourceInstanceKey, ContentBlockRevisionFact,
    ContentBlockRevisionValue, ContractCompleteness, CoverageComparison,
    EffectiveStateQualifiedValue, EffectiveStateRevisionFact, ExternalEntityRef, FactRevisionId,
    MessageRevisionFact, NativeIdentityClaim, NativeRuntimeMarkerProvenance,
    NativeRuntimeMarkerRevisionFact, NativeRuntimeMarkerValue, PlanRevisionFact,
    QualifiedTimestamp, QualifiedValue, QualifiedValueQuality, SemanticRevisionRef,
    SourceCoverageSet, SourceRecordId, TaskRevisionFact, TimestampQuality, ToolRevisionFact,
    UsageBucketsV2, UsageResponseIdentity, UsageRevisionV2Fact, UsageValueAuthority,
    UsageValueProvenance, UserInputRequestRevisionFact,
};

pub(crate) const MAX_SEMANTIC_FIXTURE_JSON_BYTES: usize = 1024 * 1024;
const MAX_SEMANTIC_FIXTURE_DEPTH: usize = 16;
const MAX_SEMANTIC_FIXTURE_NODES: usize = 4_096;
const RFC012A_FIXTURE_CONTRACT_VERSION: u32 = 1;
const RFC012C_FIXTURE_CONTRACT_VERSION: u32 = 1;
const RUNTIME_SEMANTIC_CONTRACT_VERSION: u32 = 1;
const FAMILY_VERSION: u32 = 1;
const ACTOR_RUN_FAMILY: &str = "runtime.actor-run";
const ACTOR_AFFILIATION_FAMILY: &str = "runtime.actor-affiliation";
const USAGE_V2_FAMILY: &str = "runtime.usage-v2";
const EFFECTIVE_STATE_FAMILY: &str = "runtime.effective-state";
const USER_INPUT_FAMILY: &str = "runtime.user-input-request";
const MESSAGE_FAMILY: &str = "runtime.message";
const CONTENT_BLOCK_FAMILY: &str = "runtime.content-block";
const NATIVE_MARKER_FAMILY: &str = "runtime.native-marker";
const PLAN_FAMILY: &str = "runtime.plan";
const TASK_FAMILY: &str = "runtime.task";
const TOOL_FAMILY: &str = "runtime.tool";
const MAX_INTERACTION_QUESTIONS: usize = 32;
const MAX_INTERACTION_OPTIONS: usize = 32;
const MAX_MESSAGE_CONTENT_BLOCKS: usize = 32;
const MAX_PLAN_STEPS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct SemanticFixtureError {
    message: String,
}

impl SemanticFixtureError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rfc012aCoverageExpectedWire {
    dominant_vs_baseline: CoverageComparison,
    baseline_vs_dominant: CoverageComparison,
    reset_vs_baseline: CoverageComparison,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rfc012aCoverageWire {
    baseline: SourceCoverageSet,
    dominant: SourceCoverageSet,
    reset: SourceCoverageSet,
    expected: Rfc012aCoverageExpectedWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rfc012aFixtureWire {
    fixture_contract_version: u32,
    canonical_source_instance_key: CanonicalSourceInstanceKey,
    external_entity_ref: ExternalEntityRef,
    native_identity_claim: NativeIdentityClaim,
    semantic_revision_ref: SemanticRevisionRef,
    qualified_known_zero: QualifiedValue<u64>,
    qualified_unknown: QualifiedValue<String>,
    coverage: Rfc012aCoverageWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FamilyVersionWire {
    pub family: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionIdentityWire {
    pub entity_key: CanonicalEntityKey,
    pub external_ref: ExternalEntityRef,
    pub native_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceWire {
    pub adapter_id: String,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub session: SessionIdentityWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActorExampleWire {
    pub family: String,
    pub family_version: u32,
    #[serde(deserialize_with = "strict_actor_run_revision")]
    pub revision: ActorRunRevisionFact,
    pub semantic_revision_key_hex: String,
    pub fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActorsWire {
    pub root: ActorExampleWire,
    pub child: ActorExampleWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AffiliationExampleWire {
    pub family: String,
    pub family_version: u32,
    #[serde(deserialize_with = "strict_actor_affiliation_revision")]
    pub revision: ActorAffiliationRevisionFact,
    pub semantic_revision_key_hex: String,
    pub fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AffiliationsWire {
    pub child_team_present: AffiliationExampleWire,
    pub child_workflow_present: AffiliationExampleWire,
    pub child_workflow_removed: AffiliationExampleWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageExampleWire {
    pub family: String,
    pub family_version: u32,
    #[serde(deserialize_with = "strict_usage_revision")]
    pub revision: UsageRevisionV2Fact,
    pub semantic_revision_key_hex: String,
    pub fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageAbaWire {
    pub native_message_id: String,
    pub a: UsageExampleWire,
    pub b: UsageExampleWire,
    pub a_repeat: UsageExampleWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageWire {
    pub native_message: UsageExampleWire,
    pub source_record_fallback: UsageExampleWire,
    pub response_revisions: UsageAbaWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeContractFixtureWire {
    pub fixture_contract_version: u32,
    pub runtime_semantic_contract_version: u32,
    pub families: Vec<FamilyVersionWire>,
    pub source: SourceWire,
    pub actors: ActorsWire,
    pub affiliations: AffiliationsWire,
    pub usage: UsageWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EffectiveStateDimension {
    Model,
    Effort,
    SessionMode,
    PermissionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EffectiveStateEvidenceKind {
    ConfiguredIntent,
    ResponseObserved,
    NativeTransition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EffectiveStateOperation {
    Upsert,
    Retract,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectiveStateSlotWire {
    pub completeness: ContractCompleteness,
    pub evidence_kind: EffectiveStateEvidenceKind,
    pub operation: EffectiveStateOperation,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub value: EffectiveStateQualifiedValue<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectiveStateFixtureWire {
    pub adapter_id: String,
    pub family: String,
    pub family_version: u32,
    pub fixture_contract_version: u32,
    pub runtime_semantic_contract_version: u32,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub dimension: EffectiveStateDimension,
    pub fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub configured: EffectiveStateSlotWire,
    pub observed: EffectiveStateSlotWire,
    pub retract: EffectiveStateSlotWire,
}

pub(crate) use crate::adapter::{
    ToolRevisionKind, UserInputKind, UserInputLifecycleState, UserInputOperation, UserInputQuestion,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InteractionLifecycleSlotWire {
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub result_reference: Option<String>,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub state: UserInputLifecycleState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InteractionFixtureWire {
    pub adapter_id: String,
    pub actor_run: CanonicalEntityKey,
    pub family: String,
    pub family_version: u32,
    pub fixture_contract_version: u32,
    pub kind: UserInputKind,
    pub native_tool_use_id: String,
    pub fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub pending: InteractionLifecycleSlotWire,
    pub resolved: InteractionLifecycleSlotWire,
    pub failed: InteractionLifecycleSlotWire,
    pub cancelled: InteractionLifecycleSlotWire,
    pub retract: InteractionLifecycleSlotWire,
    pub partial: InteractionLifecycleSlotWire,
    pub questions: Vec<UserInputQuestion>,
    pub runtime_semantic_contract_version: u32,
    pub session: CanonicalEntityKey,
    pub source_instance_key: CanonicalSourceInstanceKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageRevisionRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageRevisionSlotWire {
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub ordered_content_block_keys: Vec<String>,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentBlockRevisionSlotWire {
    pub ordinal: u32,
    pub content: ContentBlockRevisionValue,
    pub native_tool_call_or_result_id: Option<String>,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentBlockFixtureWire {
    pub family: String,
    pub family_version: u32,
    pub native_content_block_id: String,
    pub fact_id: CanonicalFactId,
    pub current: ContentBlockRevisionSlotWire,
    pub correction: ContentBlockRevisionSlotWire,
    pub retract: ContentBlockRevisionSlotWire,
    pub partial_retract: ContentBlockRevisionSlotWire,
}

fn deserialize_optional_content_block<'de, D>(
    deserializer: D,
) -> Result<Option<ContentBlockFixtureWire>, D::Error>
where
    D: Deserializer<'de>,
{
    ContentBlockFixtureWire::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageFixtureWire {
    pub adapter_id: String,
    pub actor_run: CanonicalEntityKey,
    pub family: String,
    pub family_version: u32,
    pub fixture_contract_version: u32,
    pub native_message_id: String,
    pub fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub role: MessageRevisionRole,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_content_block",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_block: Option<ContentBlockFixtureWire>,
    pub current: MessageRevisionSlotWire,
    pub correction: MessageRevisionSlotWire,
    pub complete_blocks: MessageRevisionSlotWire,
    pub partial_blocks: MessageRevisionSlotWire,
    pub retract: MessageRevisionSlotWire,
    pub partial: MessageRevisionSlotWire,
    pub runtime_semantic_contract_version: u32,
    pub session: CanonicalEntityKey,
    pub source_instance_key: CanonicalSourceInstanceKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeMarkerRevisionSlotWire {
    pub correlated_native_id: Option<String>,
    pub value: NativeRuntimeMarkerValue,
    pub quality: QualifiedValueQuality,
    pub effective_at: Option<i64>,
    pub provenance: NativeRuntimeMarkerProvenance,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeMarkerExampleWire {
    pub native_marker_id: String,
    pub fact_id: CanonicalFactId,
    pub current: NativeMarkerRevisionSlotWire,
    pub correction: NativeMarkerRevisionSlotWire,
    pub retract: NativeMarkerRevisionSlotWire,
    pub partial: NativeMarkerRevisionSlotWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeMarkerFixtureWire {
    pub adapter_id: String,
    pub actor_run: CanonicalEntityKey,
    pub family: String,
    pub family_version: u32,
    pub fixture_contract_version: u32,
    pub runtime_semantic_contract_version: u32,
    pub session: CanonicalEntityKey,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub source_record_id: SourceRecordId,
    pub compaction: NativeMarkerExampleWire,
    pub progress: NativeMarkerExampleWire,
    pub queue: NativeMarkerExampleWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskLifecycleState {
    Created,
    Updated,
    Completed,
    Failed,
    Cancelled,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskRevisionSlotWire {
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub owned_set: Option<Vec<String>>,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub state: TaskLifecycleState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskFixtureWire {
    pub adapter_id: String,
    pub actor_run: CanonicalEntityKey,
    pub family: String,
    pub family_version: u32,
    pub fixture_contract_version: u32,
    pub native_task_id: String,
    pub peer_native_task_id: String,
    pub fact_id: CanonicalFactId,
    pub peer_fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub subject: String,
    pub created: TaskRevisionSlotWire,
    pub updated: TaskRevisionSlotWire,
    pub completed: TaskRevisionSlotWire,
    pub retract: TaskRevisionSlotWire,
    pub partial: TaskRevisionSlotWire,
    pub collection_omit: TaskRevisionSlotWire,
    pub runtime_semantic_contract_version: u32,
    pub session: CanonicalEntityKey,
    pub source_instance_key: CanonicalSourceInstanceKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanRevisionSlotWire {
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub ordered_step_keys: Vec<String>,
    pub owned_set: Option<Vec<String>>,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanFixtureWire {
    pub adapter_id: String,
    pub actor_run: CanonicalEntityKey,
    pub family: String,
    pub family_version: u32,
    pub fixture_contract_version: u32,
    pub native_plan_id: String,
    pub peer_native_plan_id: String,
    pub fact_id: CanonicalFactId,
    pub peer_fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub subject: String,
    pub current: PlanRevisionSlotWire,
    pub complete_steps: PlanRevisionSlotWire,
    pub partial_steps: PlanRevisionSlotWire,
    pub retract: PlanRevisionSlotWire,
    pub partial: PlanRevisionSlotWire,
    pub collection_omit: PlanRevisionSlotWire,
    pub runtime_semantic_contract_version: u32,
    pub session: CanonicalEntityKey,
    pub source_instance_key: CanonicalSourceInstanceKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolRevisionSlotWire {
    pub completeness: ContractCompleteness,
    pub correlated_native_id: Option<String>,
    pub kind: ToolRevisionKind,
    pub operation: UserInputOperation,
    pub semantic_revision_key_hex: String,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolFixtureWire {
    pub adapter_id: String,
    pub actor_run: CanonicalEntityKey,
    pub family: String,
    pub family_version: u32,
    pub fixture_contract_version: u32,
    pub native_call_id: String,
    pub native_result_id: String,
    pub fact_id: CanonicalFactId,
    pub result_fact_id: CanonicalFactId,
    pub source_record_id: SourceRecordId,
    pub tool_name: String,
    pub call: ToolRevisionSlotWire,
    pub unmatched_result: ToolRevisionSlotWire,
    pub correlated_call: ToolRevisionSlotWire,
    pub correlated_result: ToolRevisionSlotWire,
    pub retract: ToolRevisionSlotWire,
    pub partial: ToolRevisionSlotWire,
    pub runtime_semantic_contract_version: u32,
    pub session: CanonicalEntityKey,
    pub source_instance_key: CanonicalSourceInstanceKey,
}

fn account_json_graph(
    value: &serde_json::Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), SemanticFixtureError> {
    if depth > MAX_SEMANTIC_FIXTURE_DEPTH {
        return Err(SemanticFixtureError::invalid(format!(
            "semantic fixture JSON exceeds depth {MAX_SEMANTIC_FIXTURE_DEPTH}"
        )));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_SEMANTIC_FIXTURE_NODES {
        return Err(SemanticFixtureError::invalid(format!(
            "semantic fixture JSON exceeds {MAX_SEMANTIC_FIXTURE_NODES} nodes"
        )));
    }
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                account_json_graph(item, depth.saturating_add(1), nodes)?;
            }
        }
        serde_json::Value::Object(fields) => {
            for child in fields.values() {
                account_json_graph(child, depth.saturating_add(1), nodes)?;
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
    Ok(())
}

fn preflight_json(json: &str) -> Result<serde_json::Value, SemanticFixtureError> {
    if json.is_empty() {
        return Err(SemanticFixtureError::invalid(
            "semantic fixture JSON must not be empty",
        ));
    }
    if json.len() > MAX_SEMANTIC_FIXTURE_JSON_BYTES {
        return Err(SemanticFixtureError::invalid(format!(
            "semantic fixture JSON exceeds {MAX_SEMANTIC_FIXTURE_JSON_BYTES} bytes"
        )));
    }
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    let mut nodes = 0;
    account_json_graph(&value, 1, &mut nodes)?;
    Ok(value)
}

fn decode_json<T>(json: &str) -> Result<T, SemanticFixtureError>
where
    T: for<'de> Deserialize<'de>,
{
    let value = preflight_json(json)?;
    serde_json::from_value(value).map_err(|error| SemanticFixtureError::invalid(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualifiedTimestampWire {
    value: String,
    quality: TimestampQuality,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageProvenanceWire {
    native_field: String,
    normalization_contract_version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageBucketsWire {
    input_tokens: QualifiedValue<u64, UsageValueAuthority, UsageProvenanceWire>,
    output_tokens: QualifiedValue<u64, UsageValueAuthority, UsageProvenanceWire>,
    cache_creation_input_tokens: QualifiedValue<u64, UsageValueAuthority, UsageProvenanceWire>,
    cache_read_input_tokens: QualifiedValue<u64, UsageValueAuthority, UsageProvenanceWire>,
}

fn usage_qualified<T>(
    value: QualifiedValue<T, UsageValueAuthority, UsageProvenanceWire>,
) -> Result<crate::adapter::UsageQualifiedValue<T>, SemanticFixtureError> {
    QualifiedValue::from_parts(
        value.value,
        value.quality,
        value.authority,
        value.completeness,
        value.unknown_reason,
        value.effective_at,
        UsageValueProvenance {
            native_field: value.provenance.native_field,
            normalization_contract_version: value.provenance.normalization_contract_version,
        },
    )
    .map_err(|error| SemanticFixtureError::invalid(error.to_string()))
}

fn optional_timestamp(value: Option<QualifiedTimestampWire>) -> Option<QualifiedTimestamp> {
    value.map(|timestamp| QualifiedTimestamp {
        value: timestamp.value,
        quality: timestamp.quality,
    })
}

fn strict_actor_run_revision<'de, D>(deserializer: D) -> Result<ActorRunRevisionFact, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        actor_run: CanonicalEntityKey,
        session: CanonicalEntityKey,
        role: ActorRunRole,
        parent_actor_run: Option<CanonicalEntityKey>,
        native_session_id: Option<String>,
        native_actor_id: Option<String>,
        native_actor_type: Option<String>,
    }

    let wire = Wire::deserialize(deserializer)?;
    Ok(ActorRunRevisionFact {
        actor_run: wire.actor_run,
        session: wire.session,
        role: wire.role,
        parent_actor_run: wire.parent_actor_run,
        native_session_id: wire.native_session_id,
        native_actor_id: wire.native_actor_id,
        native_actor_type: wire.native_actor_type,
    })
}

fn strict_actor_affiliation_revision<'de, D>(
    deserializer: D,
) -> Result<ActorAffiliationRevisionFact, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        affiliation: CanonicalEntityKey,
        actor_run: CanonicalEntityKey,
        session: CanonicalEntityKey,
        dimension: ActorAffiliationDimension,
        target: CanonicalEntityKey,
        member: Option<CanonicalEntityKey>,
        native_target_id: Option<String>,
        native_member_id: Option<String>,
        state: ActorAffiliationState,
        effective_at: Option<QualifiedTimestampWire>,
    }

    let wire = Wire::deserialize(deserializer)?;
    Ok(ActorAffiliationRevisionFact {
        affiliation: wire.affiliation,
        actor_run: wire.actor_run,
        session: wire.session,
        dimension: wire.dimension,
        target: wire.target,
        member: wire.member,
        native_target_id: wire.native_target_id,
        native_member_id: wire.native_member_id,
        state: wire.state,
        effective_at: optional_timestamp(wire.effective_at),
    })
}

fn strict_usage_revision<'de, D>(deserializer: D) -> Result<UsageRevisionV2Fact, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        session: CanonicalEntityKey,
        actor_run: CanonicalEntityKey,
        #[serde(with = "strict_standard_base64")]
        response_key: Vec<u8>,
        response_identity: UsageResponseIdentity,
        native_message_id: Option<String>,
        request_id: Option<String>,
        buckets: UsageBucketsWire,
        model: Option<QualifiedValue<String, UsageValueAuthority, UsageProvenanceWire>>,
        effort: Option<QualifiedValue<String, UsageValueAuthority, UsageProvenanceWire>>,
        source_time: Option<QualifiedTimestampWire>,
    }

    let wire = Wire::deserialize(deserializer)?;
    let buckets = UsageBucketsV2 {
        input_tokens: usage_qualified(wire.buckets.input_tokens).map_err(D::Error::custom)?,
        output_tokens: usage_qualified(wire.buckets.output_tokens).map_err(D::Error::custom)?,
        cache_creation_input_tokens: usage_qualified(wire.buckets.cache_creation_input_tokens)
            .map_err(D::Error::custom)?,
        cache_read_input_tokens: usage_qualified(wire.buckets.cache_read_input_tokens)
            .map_err(D::Error::custom)?,
    };
    let model = match wire.model {
        Some(value) => Some(usage_qualified(value).map_err(D::Error::custom)?),
        None => None,
    };
    let effort = match wire.effort {
        Some(value) => Some(usage_qualified(value).map_err(D::Error::custom)?),
        None => None,
    };
    Ok(UsageRevisionV2Fact {
        session: wire.session,
        actor_run: wire.actor_run,
        response_key: wire.response_key,
        response_identity: wire.response_identity,
        native_message_id: wire.native_message_id,
        request_id: wire.request_id,
        buckets,
        model,
        effort,
        source_time: optional_timestamp(wire.source_time),
    })
}

const MAX_USAGE_RESPONSE_KEY_BYTES: usize = 8 * 1024;
const MAX_USAGE_RESPONSE_KEY_BASE64_CHARS: usize = MAX_USAGE_RESPONSE_KEY_BYTES.div_ceil(3) * 4;

mod strict_standard_base64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() > super::MAX_USAGE_RESPONSE_KEY_BASE64_CHARS {
            return Err(serde::de::Error::custom(
                "response_key exceeds the bounded encoded base64 maximum",
            ));
        }
        STANDARD.decode(value).map_err(serde::de::Error::custom)
    }
}

fn encode_json<T>(value: &T) -> Result<String, SemanticFixtureError>
where
    T: Serialize,
{
    serde_json::to_string(value).map_err(|error| SemanticFixtureError::invalid(error.to_string()))
}

const MAX_ADAPTER_ID_BYTES: usize = 128;
const MAX_AUTHORITY_BYTES: usize = 256;
const MAX_RUNTIME_SEMANTIC_TEXT_BYTES: usize = 8 * 1024;

fn validate_canonical_source_string(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), SemanticFixtureError> {
    if value.is_empty() || value.trim() != value {
        return Err(SemanticFixtureError::invalid(format!(
            "{label} must be a non-empty canonical string"
        )));
    }
    if value.len() > max_bytes {
        return Err(SemanticFixtureError::invalid(format!(
            "{label} exceeds {max_bytes} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_hex_digest(value: &str, label: &str) -> Result<[u8; 32], SemanticFixtureError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SemanticFixtureError::invalid(format!(
            "{label} must be 32 lowercase hex bytes"
        )));
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(SemanticFixtureError::invalid(format!(
            "{label} must be 32 lowercase hex bytes"
        )));
    }
    let mut digest = [0_u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16)
            .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    }
    Ok(digest)
}

fn verify_semantic_revision(
    family: &str,
    family_version: u32,
    expected_family: &str,
    semantic_revision_key_hex: &str,
    recomputed: [u8; 32],
    fact_id: &CanonicalFactId,
    semantic_revision_ref: &SemanticRevisionRef,
) -> Result<(), SemanticFixtureError> {
    if family != expected_family {
        return Err(SemanticFixtureError::invalid(format!(
            "example family must be {expected_family}"
        )));
    }
    if family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(format!(
            "unsupported {expected_family} version"
        )));
    }
    parse_hex_digest(semantic_revision_key_hex, "semantic_revision_key_hex")?;
    if hex_digest(&recomputed) != semantic_revision_key_hex {
        return Err(SemanticFixtureError::invalid(format!(
            "{expected_family} semantic revision key mismatch"
        )));
    }
    let expected_ref = SemanticRevisionRef::new(
        FactRevisionId::derive(fact_id, 1, &recomputed)
            .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?,
    );
    if expected_ref != *semantic_revision_ref {
        return Err(SemanticFixtureError::invalid(format!(
            "{expected_family} semantic revision ref mismatch"
        )));
    }
    Ok(())
}

fn validate_rfc012a_fixture(fixture: &Rfc012aFixtureWire) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012A_FIXTURE_CONTRACT_VERSION {
        return Err(SemanticFixtureError::invalid(
            "unsupported RFC 012A fixture contract version",
        ));
    }
    if fixture.canonical_source_instance_key != fixture.coverage.baseline.scope.source_instance_key
        || fixture.canonical_source_instance_key
            != fixture.coverage.dominant.scope.source_instance_key
        || fixture.canonical_source_instance_key != fixture.coverage.reset.scope.source_instance_key
    {
        return Err(SemanticFixtureError::invalid(
            "coverage scopes must use the fixture source instance",
        ));
    }
    if fixture.native_identity_claim.entity_ref != fixture.external_entity_ref {
        return Err(SemanticFixtureError::invalid(
            "native identity claim must use the fixture external entity reference",
        ));
    }
    if fixture.qualified_known_zero.value != Some(0) {
        return Err(SemanticFixtureError::invalid(
            "qualified known zero must preserve exact zero",
        ));
    }
    if fixture.qualified_unknown.value.is_some() {
        return Err(SemanticFixtureError::invalid(
            "qualified unknown must keep a null value",
        ));
    }
    validate_canonical_source_string(
        "qualified known zero authority",
        &fixture.qualified_known_zero.authority,
        MAX_AUTHORITY_BYTES,
    )?;
    validate_canonical_source_string(
        "qualified unknown authority",
        &fixture.qualified_unknown.authority,
        MAX_AUTHORITY_BYTES,
    )?;
    if fixture.native_identity_claim.identity.provenance.as_slice()
        != std::slice::from_ref(&fixture.semantic_revision_ref)
    {
        return Err(SemanticFixtureError::invalid(
            "native identity provenance must bind the fixture semantic revision reference",
        ));
    }
    if fixture.qualified_known_zero.provenance.as_slice()
        != std::slice::from_ref(&fixture.semantic_revision_ref)
    {
        return Err(SemanticFixtureError::invalid(
            "qualified known zero provenance must bind the fixture semantic revision reference",
        ));
    }
    if !fixture.qualified_unknown.provenance.is_empty() {
        return Err(SemanticFixtureError::invalid(
            "qualified unknown provenance must remain empty",
        ));
    }
    let dominant_vs_baseline =
        compare_coverage(&fixture.coverage.dominant, &fixture.coverage.baseline)
            .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    let baseline_vs_dominant =
        compare_coverage(&fixture.coverage.baseline, &fixture.coverage.dominant)
            .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    let reset_vs_baseline = compare_coverage(&fixture.coverage.reset, &fixture.coverage.baseline)
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    if dominant_vs_baseline != fixture.coverage.expected.dominant_vs_baseline
        || baseline_vs_dominant != fixture.coverage.expected.baseline_vs_dominant
        || reset_vs_baseline != fixture.coverage.expected.reset_vs_baseline
    {
        return Err(SemanticFixtureError::invalid(
            "coverage comparison outcomes do not match the fixture",
        ));
    }
    Ok(())
}

fn validate_actor_example(
    example: &ActorExampleWire,
    expected_role: ActorRunRole,
) -> Result<(), SemanticFixtureError> {
    example
        .revision
        .validate()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    if example.revision.role != expected_role {
        return Err(SemanticFixtureError::invalid(
            "runtime fixture must include one root actor and one child actor",
        ));
    }
    let recomputed = example
        .revision
        .semantic_revision_key()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    verify_semantic_revision(
        &example.family,
        example.family_version,
        ACTOR_RUN_FAMILY,
        &example.semantic_revision_key_hex,
        recomputed,
        &example.fact_id,
        &example.semantic_revision_ref,
    )
}

fn validate_affiliation_example(
    example: &AffiliationExampleWire,
) -> Result<(), SemanticFixtureError> {
    example
        .revision
        .validate()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    let recomputed = example
        .revision
        .semantic_revision_key()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    verify_semantic_revision(
        &example.family,
        example.family_version,
        ACTOR_AFFILIATION_FAMILY,
        &example.semantic_revision_key_hex,
        recomputed,
        &example.fact_id,
        &example.semantic_revision_ref,
    )
}

fn validate_usage_example(example: &UsageExampleWire) -> Result<(), SemanticFixtureError> {
    example
        .revision
        .validate()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    let recomputed = example
        .revision
        .semantic_revision_key()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    verify_semantic_revision(
        &example.family,
        example.family_version,
        USAGE_V2_FAMILY,
        &example.semantic_revision_key_hex,
        recomputed,
        &example.fact_id,
        &example.semantic_revision_ref,
    )
}

fn validate_rfc012c_fixture(
    fixture: &RuntimeContractFixtureWire,
) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION {
        return Err(SemanticFixtureError::invalid(
            "unsupported runtime fixture contract version",
        ));
    }
    if fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION {
        return Err(SemanticFixtureError::invalid(
            "unsupported runtime semantic contract version",
        ));
    }
    let families: Vec<(&str, u32)> = fixture
        .families
        .iter()
        .map(|family| (family.family.as_str(), family.version))
        .collect();
    if families
        != vec![
            (ACTOR_RUN_FAMILY, FAMILY_VERSION),
            (ACTOR_AFFILIATION_FAMILY, FAMILY_VERSION),
            (USAGE_V2_FAMILY, FAMILY_VERSION),
        ]
    {
        return Err(SemanticFixtureError::invalid(
            "runtime fixture families must be actor-run, actor-affiliation, and usage-v2 v1",
        ));
    }
    if fixture.source.session.external_ref.entity_key != fixture.source.session.entity_key {
        return Err(SemanticFixtureError::invalid(
            "session external reference must match the session entity key",
        ));
    }
    validate_canonical_source_string(
        "adapter_id",
        &fixture.source.adapter_id,
        MAX_ADAPTER_ID_BYTES,
    )?;
    validate_canonical_source_string(
        "native_session_id",
        &fixture.source.session.native_session_id,
        MAX_RUNTIME_SEMANTIC_TEXT_BYTES,
    )?;
    validate_actor_example(&fixture.actors.root, ActorRunRole::Root)?;
    validate_actor_example(&fixture.actors.child, ActorRunRole::Child)?;
    if fixture.actors.child.revision.parent_actor_run
        != Some(fixture.actors.root.revision.actor_run)
    {
        return Err(SemanticFixtureError::invalid(
            "child actor must be parented to the fixture root actor",
        ));
    }
    if fixture.actors.root.revision.session != fixture.source.session.entity_key
        || fixture.actors.child.revision.session != fixture.source.session.entity_key
    {
        return Err(SemanticFixtureError::invalid(
            "fixture actors must reference the fixture session",
        ));
    }
    validate_affiliation_example(&fixture.affiliations.child_team_present)?;
    validate_affiliation_example(&fixture.affiliations.child_workflow_present)?;
    validate_affiliation_example(&fixture.affiliations.child_workflow_removed)?;
    let team = &fixture.affiliations.child_team_present.revision;
    let workflow = &fixture.affiliations.child_workflow_present.revision;
    let removed = &fixture.affiliations.child_workflow_removed.revision;
    if team.dimension != ActorAffiliationDimension::Team
        || workflow.dimension != ActorAffiliationDimension::Workflow
        || removed.dimension != ActorAffiliationDimension::Workflow
    {
        return Err(SemanticFixtureError::invalid(
            "fixture affiliations must keep team and workflow dimensions orthogonal",
        ));
    }
    if team.actor_run != fixture.actors.child.revision.actor_run
        || workflow.actor_run != fixture.actors.child.revision.actor_run
        || removed.actor_run != fixture.actors.child.revision.actor_run
    {
        return Err(SemanticFixtureError::invalid(
            "team and workflow affiliations must attach to the same child actor",
        ));
    }
    if team.session != fixture.source.session.entity_key
        || workflow.session != fixture.source.session.entity_key
        || removed.session != fixture.source.session.entity_key
    {
        return Err(SemanticFixtureError::invalid(
            "fixture affiliations must reference the fixture session",
        ));
    }
    if removed.state != ActorAffiliationState::Removed {
        return Err(SemanticFixtureError::invalid(
            "fixture must include a removed affiliation revision",
        ));
    }
    if workflow.affiliation != removed.affiliation
        || workflow.target != removed.target
        || workflow.member != removed.member
        || fixture.affiliations.child_workflow_present.fact_id
            != fixture.affiliations.child_workflow_removed.fact_id
    {
        return Err(SemanticFixtureError::invalid(
            "workflow removal must revise the same affiliation identity",
        ));
    }
    if fixture
        .affiliations
        .child_workflow_present
        .semantic_revision_ref
        == fixture
            .affiliations
            .child_workflow_removed
            .semantic_revision_ref
    {
        return Err(SemanticFixtureError::invalid(
            "workflow removal must mint a distinct semantic revision",
        ));
    }
    validate_usage_example(&fixture.usage.native_message)?;
    validate_usage_example(&fixture.usage.source_record_fallback)?;
    validate_usage_example(&fixture.usage.response_revisions.a)?;
    validate_usage_example(&fixture.usage.response_revisions.b)?;
    validate_usage_example(&fixture.usage.response_revisions.a_repeat)?;
    if fixture.usage.native_message.revision.response_identity
        != UsageResponseIdentity::NativeMessageId
    {
        return Err(SemanticFixtureError::invalid(
            "native usage example must use a native message identity",
        ));
    }
    if fixture
        .usage
        .source_record_fallback
        .revision
        .response_identity
        != UsageResponseIdentity::SourceRecordFallback
    {
        return Err(SemanticFixtureError::invalid(
            "fallback usage example must use a source-record identity",
        ));
    }
    let aba = &fixture.usage.response_revisions;
    if aba.a.semantic_revision_ref != aba.a_repeat.semantic_revision_ref
        || aba.a.semantic_revision_key_hex != aba.a_repeat.semantic_revision_key_hex
    {
        return Err(SemanticFixtureError::invalid(
            "A and A-repeat usage revisions must share semantic identity",
        ));
    }
    if aba.a.semantic_revision_ref == aba.b.semantic_revision_ref {
        return Err(SemanticFixtureError::invalid(
            "A and B usage revisions must have distinct semantic identity",
        ));
    }
    if aba.a.fact_id != aba.b.fact_id || aba.a.fact_id != aba.a_repeat.fact_id {
        return Err(SemanticFixtureError::invalid(
            "A -> B -> A usage revisions must share one response fact identity",
        ));
    }
    if aba.a.revision.native_message_id.as_deref() != Some(aba.native_message_id.as_str())
        || aba.b.revision.native_message_id.as_deref() != Some(aba.native_message_id.as_str())
        || aba.a_repeat.revision.native_message_id.as_deref()
            != Some(aba.native_message_id.as_str())
    {
        return Err(SemanticFixtureError::invalid(
            "A -> B -> A revisions must use the declared native message ID",
        ));
    }
    for example in [
        &fixture.usage.native_message,
        &fixture.usage.source_record_fallback,
        &aba.a,
        &aba.b,
        &aba.a_repeat,
    ] {
        if example.revision.session != fixture.source.session.entity_key {
            return Err(SemanticFixtureError::invalid(
                "fixture usage revisions must reference the fixture session",
            ));
        }
        if example.revision.actor_run != fixture.actors.child.revision.actor_run
            && example.revision.actor_run != fixture.actors.root.revision.actor_run
        {
            return Err(SemanticFixtureError::invalid(
                "fixture usage revisions must reference a fixture actor",
            ));
        }
    }
    let source_record_id = fixture.actors.root.source_record_id;
    for example_id in [
        fixture.actors.child.source_record_id,
        fixture.affiliations.child_team_present.source_record_id,
        fixture.affiliations.child_workflow_present.source_record_id,
        fixture.affiliations.child_workflow_removed.source_record_id,
        fixture.usage.native_message.source_record_id,
        fixture.usage.source_record_fallback.source_record_id,
        aba.a.source_record_id,
        aba.b.source_record_id,
        aba.a_repeat.source_record_id,
    ] {
        if example_id != source_record_id {
            return Err(SemanticFixtureError::invalid(
                "fixture examples must share one source-record identity",
            ));
        }
    }
    Ok(())
}

pub(crate) fn parse_rfc012a_v1_json(json: &str) -> Result<String, SemanticFixtureError> {
    let fixture: Rfc012aFixtureWire = decode_json(json)?;
    validate_rfc012a_fixture(&fixture)?;
    encode_json(&fixture)
}

pub(crate) fn parse_rfc012c_runtime_v1_json(json: &str) -> Result<String, SemanticFixtureError> {
    let fixture: RuntimeContractFixtureWire = decode_json(json)?;
    validate_rfc012c_fixture(&fixture)?;
    encode_json(&fixture)
}

fn effective_state_native_bytes(dimension: EffectiveStateDimension) -> &'static [u8] {
    match dimension {
        EffectiveStateDimension::Model => b"model",
        EffectiveStateDimension::Effort => b"effort",
        EffectiveStateDimension::SessionMode => b"session_mode",
        EffectiveStateDimension::PermissionMode => b"permission_mode",
    }
}

fn verify_fact_revision_identity(
    family: &str,
    fact_id: &CanonicalFactId,
    semantic_revision_key_hex: &str,
    semantic_revision_ref: &SemanticRevisionRef,
    revision_key: Result<[u8; 32], AdapterError>,
) -> Result<(), SemanticFixtureError> {
    let revision_key =
        revision_key.map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    verify_semantic_revision(
        family,
        FAMILY_VERSION,
        family,
        semantic_revision_key_hex,
        revision_key,
        fact_id,
        semantic_revision_ref,
    )
}

pub(crate) fn effective_state_revision(
    fixture: &EffectiveStateFixtureWire,
    slot: &EffectiveStateSlotWire,
) -> EffectiveStateRevisionFact {
    EffectiveStateRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        dimension: match fixture.dimension {
            EffectiveStateDimension::Model => crate::adapter::EffectiveStateDimension::Model,
            EffectiveStateDimension::Effort => crate::adapter::EffectiveStateDimension::Effort,
            EffectiveStateDimension::SessionMode => {
                crate::adapter::EffectiveStateDimension::SessionMode
            }
            EffectiveStateDimension::PermissionMode => {
                crate::adapter::EffectiveStateDimension::PermissionMode
            }
        },
        value: slot.value.clone(),
        evidence_kind: match slot.evidence_kind {
            EffectiveStateEvidenceKind::ConfiguredIntent => {
                crate::adapter::EffectiveStateEvidenceKind::ConfiguredIntent
            }
            EffectiveStateEvidenceKind::ResponseObserved => {
                crate::adapter::EffectiveStateEvidenceKind::ResponseObserved
            }
            EffectiveStateEvidenceKind::NativeTransition => {
                crate::adapter::EffectiveStateEvidenceKind::NativeTransition
            }
        },
        completeness: slot.completeness,
        operation: match slot.operation {
            EffectiveStateOperation::Upsert => UserInputOperation::Upsert,
            EffectiveStateOperation::Retract => UserInputOperation::Retract,
        },
    }
}

fn interaction_revision(
    fixture: &InteractionFixtureWire,
    slot: &InteractionLifecycleSlotWire,
) -> UserInputRequestRevisionFact {
    UserInputRequestRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        native_tool_use_id: fixture.native_tool_use_id.clone(),
        kind: fixture.kind,
        questions: fixture.questions.clone(),
        state: slot.state,
        operation: slot.operation,
        completeness: slot.completeness,
        result_reference: slot.result_reference.clone(),
    }
}

fn message_revision(
    fixture: &MessageFixtureWire,
    slot: &MessageRevisionSlotWire,
) -> MessageRevisionFact {
    MessageRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        native_message_id: fixture.native_message_id.clone(),
        role: match fixture.role {
            MessageRevisionRole::User => crate::adapter::MessageRevisionRole::User,
            MessageRevisionRole::Assistant => crate::adapter::MessageRevisionRole::Assistant,
            MessageRevisionRole::System => crate::adapter::MessageRevisionRole::System,
        },
        ordered_content_block_keys: slot.ordered_content_block_keys.clone(),
        completeness: slot.completeness,
        operation: slot.operation,
    }
}

pub(crate) fn content_block_revision(
    fixture: &MessageFixtureWire,
    content_block: &ContentBlockFixtureWire,
    slot: &ContentBlockRevisionSlotWire,
) -> ContentBlockRevisionFact {
    ContentBlockRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        message: fixture.fact_id,
        native_content_block_id: Some(content_block.native_content_block_id.clone()),
        ordinal: slot.ordinal,
        content: slot.content.clone(),
        native_tool_call_or_result_id: slot.native_tool_call_or_result_id.clone(),
        completeness: slot.completeness,
        operation: slot.operation,
    }
}

pub(crate) fn native_marker_revision(
    fixture: &NativeMarkerFixtureWire,
    example: &NativeMarkerExampleWire,
    slot: &NativeMarkerRevisionSlotWire,
) -> NativeRuntimeMarkerRevisionFact {
    NativeRuntimeMarkerRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        native_marker_id: example.native_marker_id.clone(),
        correlated_native_id: slot.correlated_native_id.clone(),
        value: slot.value.clone(),
        quality: slot.quality,
        effective_at: slot.effective_at,
        provenance: slot.provenance.clone(),
        completeness: slot.completeness,
        operation: slot.operation,
    }
}

fn task_revision(
    fixture: &TaskFixtureWire,
    native_task_id: &str,
    slot: &TaskRevisionSlotWire,
) -> TaskRevisionFact {
    TaskRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        native_task_id: native_task_id.to_owned(),
        subject: fixture.subject.clone(),
        state: match slot.state {
            TaskLifecycleState::Created => crate::adapter::TaskLifecycleState::Created,
            TaskLifecycleState::Updated => crate::adapter::TaskLifecycleState::Updated,
            TaskLifecycleState::Completed => crate::adapter::TaskLifecycleState::Completed,
            TaskLifecycleState::Failed => crate::adapter::TaskLifecycleState::Failed,
            TaskLifecycleState::Cancelled => crate::adapter::TaskLifecycleState::Cancelled,
            TaskLifecycleState::Removed => crate::adapter::TaskLifecycleState::Removed,
        },
        completeness: slot.completeness,
        operation: slot.operation,
        owned_set: slot.owned_set.clone(),
    }
}

fn plan_revision(
    fixture: &PlanFixtureWire,
    native_plan_id: &str,
    slot: &PlanRevisionSlotWire,
) -> PlanRevisionFact {
    PlanRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        native_plan_id: native_plan_id.to_owned(),
        subject: fixture.subject.clone(),
        ordered_step_keys: slot.ordered_step_keys.clone(),
        completeness: slot.completeness,
        operation: slot.operation,
        owned_set: slot.owned_set.clone(),
    }
}

fn tool_revision(fixture: &ToolFixtureWire, slot: &ToolRevisionSlotWire) -> ToolRevisionFact {
    ToolRevisionFact {
        session: fixture.session,
        actor_run: fixture.actor_run,
        native_tool_id: tool_slot_native_id(fixture, slot).to_owned(),
        kind: slot.kind,
        tool_name: fixture.tool_name.clone(),
        correlated_native_id: slot.correlated_native_id.clone(),
        completeness: slot.completeness,
        operation: slot.operation,
    }
}

fn verify_committed_fact_id(
    adapter_id: &str,
    source_instance_key: &CanonicalSourceInstanceKey,
    family: &str,
    native: &[u8],
    committed: &CanonicalFactId,
) -> Result<(), SemanticFixtureError> {
    let expected = CanonicalFactId::native(adapter_id, source_instance_key, family, native)
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    if expected != *committed {
        return Err(SemanticFixtureError::invalid(format!(
            "{family} fact_id does not match native derivation"
        )));
    }
    Ok(())
}

fn validate_canonical_runtime_text(label: &str, value: &str) -> Result<(), SemanticFixtureError> {
    validate_canonical_source_string(label, value, MAX_RUNTIME_SEMANTIC_TEXT_BYTES)
}

fn validate_effective_state_slot(
    fixture: &EffectiveStateFixtureWire,
    slot_name: &str,
    slot: &EffectiveStateSlotWire,
    expected_kind: EffectiveStateEvidenceKind,
    expected_operation: EffectiveStateOperation,
) -> Result<(), SemanticFixtureError> {
    if slot.evidence_kind != expected_kind {
        return Err(SemanticFixtureError::invalid(format!(
            "effective-state {slot_name} evidence_kind does not match its fixture slot"
        )));
    }
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "effective-state {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != ContractCompleteness::Complete {
        return Err(SemanticFixtureError::invalid(format!(
            "effective-state {slot_name} must be a complete replacement slot"
        )));
    }
    verify_fact_revision_identity(
        EFFECTIVE_STATE_FAMILY,
        &fixture.fact_id,
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        effective_state_revision(fixture, slot).semantic_revision_key(),
    )
}

fn validate_rfc012c_effective_state_fixture(
    fixture: &EffectiveStateFixtureWire,
) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION
        || fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "unsupported effective-state fixture contract version",
        ));
    }
    if fixture.family != EFFECTIVE_STATE_FAMILY || fixture.family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(
            "effective-state fixture family must be runtime.effective-state@1",
        ));
    }
    validate_canonical_source_string("adapter_id", &fixture.adapter_id, MAX_ADAPTER_ID_BYTES)?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        EFFECTIVE_STATE_FAMILY,
        effective_state_native_bytes(fixture.dimension),
        &fixture.fact_id,
    )?;
    validate_effective_state_slot(
        fixture,
        "configured",
        &fixture.configured,
        EffectiveStateEvidenceKind::ConfiguredIntent,
        EffectiveStateOperation::Upsert,
    )?;
    validate_effective_state_slot(
        fixture,
        "observed",
        &fixture.observed,
        match fixture.dimension {
            EffectiveStateDimension::Model | EffectiveStateDimension::Effort => {
                EffectiveStateEvidenceKind::ResponseObserved
            }
            EffectiveStateDimension::SessionMode | EffectiveStateDimension::PermissionMode => {
                EffectiveStateEvidenceKind::NativeTransition
            }
        },
        EffectiveStateOperation::Upsert,
    )?;
    validate_effective_state_slot(
        fixture,
        "retract",
        &fixture.retract,
        EffectiveStateEvidenceKind::NativeTransition,
        EffectiveStateOperation::Retract,
    )?;
    if fixture.configured.semantic_revision_ref == fixture.observed.semantic_revision_ref
        || fixture.configured.semantic_revision_ref == fixture.retract.semantic_revision_ref
        || fixture.observed.semantic_revision_ref == fixture.retract.semantic_revision_ref
    {
        return Err(SemanticFixtureError::invalid(
            "effective-state configured, observed, and retract revisions must have distinct semantic identity",
        ));
    }
    Ok(())
}

fn validate_interaction_questions(
    questions: &[UserInputQuestion],
) -> Result<(), SemanticFixtureError> {
    if questions.is_empty() || questions.len() > MAX_INTERACTION_QUESTIONS {
        return Err(SemanticFixtureError::invalid(format!(
            "interaction questions must contain 1..={MAX_INTERACTION_QUESTIONS} typed questions"
        )));
    }
    for question in questions {
        if let Some(header) = &question.header {
            validate_canonical_runtime_text("question header", header)?;
        }
        validate_canonical_runtime_text("question prompt", &question.prompt)?;
        if question.options.len() > MAX_INTERACTION_OPTIONS {
            return Err(SemanticFixtureError::invalid(format!(
                "question options exceed {MAX_INTERACTION_OPTIONS}"
            )));
        }
        for option in &question.options {
            validate_canonical_runtime_text("option label", &option.label)?;
            if let Some(description) = &option.description {
                validate_canonical_runtime_text("option description", description)?;
            }
            if let Some(preview) = &option.preview {
                validate_canonical_runtime_text("option preview", preview)?;
            }
        }
    }
    Ok(())
}

fn validate_interaction_slot(
    fixture: &InteractionFixtureWire,
    slot_name: &str,
    slot: &InteractionLifecycleSlotWire,
    expected_state: UserInputLifecycleState,
    expected_operation: UserInputOperation,
    expected_completeness: ContractCompleteness,
    require_result: bool,
) -> Result<(), SemanticFixtureError> {
    if slot.state != expected_state {
        return Err(SemanticFixtureError::invalid(
            "interaction lifecycle state does not match its fixture slot",
        ));
    }
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "interaction {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != expected_completeness {
        return Err(SemanticFixtureError::invalid(format!(
            "interaction {slot_name} completeness does not match its fixture slot"
        )));
    }
    if require_result && slot.result_reference.is_none() {
        return Err(SemanticFixtureError::invalid(
            "resolved interaction requires a typed result_reference",
        ));
    }
    if expected_state == UserInputLifecycleState::Pending && slot.result_reference.is_some() {
        return Err(SemanticFixtureError::invalid(
            "pending interaction cannot carry a result_reference",
        ));
    }
    if let Some(result_reference) = &slot.result_reference {
        validate_canonical_runtime_text("result_reference", result_reference)?;
    }
    verify_fact_revision_identity(
        USER_INPUT_FAMILY,
        &fixture.fact_id,
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        interaction_revision(fixture, slot).semantic_revision_key(),
    )
}

fn validate_rfc012c_interaction_fixture(
    fixture: &InteractionFixtureWire,
) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION
        || fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "unsupported interaction fixture contract version",
        ));
    }
    if fixture.family != USER_INPUT_FAMILY || fixture.family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(
            "interaction fixture family must be runtime.user-input-request@1",
        ));
    }
    validate_canonical_source_string("adapter_id", &fixture.adapter_id, MAX_ADAPTER_ID_BYTES)?;
    validate_canonical_runtime_text("native_tool_use_id", &fixture.native_tool_use_id)?;
    validate_interaction_questions(&fixture.questions)?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        USER_INPUT_FAMILY,
        fixture.native_tool_use_id.as_bytes(),
        &fixture.fact_id,
    )?;
    validate_interaction_slot(
        fixture,
        "pending",
        &fixture.pending,
        UserInputLifecycleState::Pending,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        false,
    )?;
    validate_interaction_slot(
        fixture,
        "resolved",
        &fixture.resolved,
        UserInputLifecycleState::Resolved,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        true,
    )?;
    validate_interaction_slot(
        fixture,
        "failed",
        &fixture.failed,
        UserInputLifecycleState::Failed,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        false,
    )?;
    validate_interaction_slot(
        fixture,
        "cancelled",
        &fixture.cancelled,
        UserInputLifecycleState::Cancelled,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        false,
    )?;
    validate_interaction_slot(
        fixture,
        "retract",
        &fixture.retract,
        UserInputLifecycleState::Pending,
        UserInputOperation::Retract,
        ContractCompleteness::Complete,
        false,
    )?;
    validate_interaction_slot(
        fixture,
        "partial",
        &fixture.partial,
        UserInputLifecycleState::Pending,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
        false,
    )?;
    let refs = [
        &fixture.pending.semantic_revision_ref,
        &fixture.resolved.semantic_revision_ref,
        &fixture.failed.semantic_revision_ref,
        &fixture.cancelled.semantic_revision_ref,
        &fixture.retract.semantic_revision_ref,
        &fixture.partial.semantic_revision_ref,
    ];
    for (index, left) in refs.iter().enumerate() {
        for right in refs.iter().skip(index + 1) {
            if left == right {
                return Err(SemanticFixtureError::invalid(
                    "interaction lifecycle slots must have distinct semantic identity",
                ));
            }
        }
    }
    Ok(())
}

fn validate_message_slot(
    fixture: &MessageFixtureWire,
    slot_name: &str,
    slot: &MessageRevisionSlotWire,
    expected_operation: UserInputOperation,
    expected_completeness: ContractCompleteness,
    expected_keys: &[&str],
) -> Result<(), SemanticFixtureError> {
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "message {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != expected_completeness {
        return Err(SemanticFixtureError::invalid(format!(
            "message {slot_name} completeness does not match its fixture slot"
        )));
    }
    if slot.ordered_content_block_keys.len() > MAX_MESSAGE_CONTENT_BLOCKS
        || slot.ordered_content_block_keys.is_empty()
    {
        return Err(SemanticFixtureError::invalid(format!(
            "message {slot_name} ordered_content_block_keys must contain 1..={MAX_MESSAGE_CONTENT_BLOCKS} keys"
        )));
    }
    if slot.ordered_content_block_keys != expected_keys {
        return Err(SemanticFixtureError::invalid(format!(
            "message {slot_name} ordered_content_block_keys do not match the declared snapshot"
        )));
    }
    let mut seen = BTreeSet::new();
    for key in &slot.ordered_content_block_keys {
        validate_canonical_runtime_text("content block key", key)?;
        if !seen.insert(key.as_str()) {
            return Err(SemanticFixtureError::invalid(
                "message content block keys must be unique in one snapshot",
            ));
        }
    }
    verify_fact_revision_identity(
        MESSAGE_FAMILY,
        &fixture.fact_id,
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        message_revision(fixture, slot).semantic_revision_key(),
    )
}

fn validate_content_block_slot(
    fixture: &MessageFixtureWire,
    content_block: &ContentBlockFixtureWire,
    slot_name: &str,
    slot: &ContentBlockRevisionSlotWire,
    expected_operation: UserInputOperation,
    expected_completeness: ContractCompleteness,
) -> Result<(), SemanticFixtureError> {
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "content-block {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != expected_completeness {
        return Err(SemanticFixtureError::invalid(format!(
            "content-block {slot_name} completeness does not match its fixture slot"
        )));
    }
    verify_fact_revision_identity(
        CONTENT_BLOCK_FAMILY,
        &content_block.fact_id,
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        content_block_revision(fixture, content_block, slot).semantic_revision_key(),
    )
}

fn validate_content_block_fixture(
    fixture: &MessageFixtureWire,
    content_block: &ContentBlockFixtureWire,
) -> Result<(), SemanticFixtureError> {
    if content_block.family != CONTENT_BLOCK_FAMILY
        || content_block.family_version != FAMILY_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "content-block fixture family must be runtime.content-block@1",
        ));
    }
    validate_canonical_runtime_text(
        "native_content_block_id",
        &content_block.native_content_block_id,
    )?;
    let stable_native_key = content_block_revision(fixture, content_block, &content_block.current)
        .stable_native_fact_key()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        CONTENT_BLOCK_FAMILY,
        &stable_native_key,
        &content_block.fact_id,
    )?;
    if content_block.fact_id == fixture.fact_id {
        return Err(SemanticFixtureError::invalid(
            "content block and parent message must have distinct fact identities",
        ));
    }
    validate_content_block_slot(
        fixture,
        content_block,
        "current",
        &content_block.current,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
    )?;
    validate_content_block_slot(
        fixture,
        content_block,
        "correction",
        &content_block.correction,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
    )?;
    validate_content_block_slot(
        fixture,
        content_block,
        "retract",
        &content_block.retract,
        UserInputOperation::Retract,
        ContractCompleteness::Complete,
    )?;
    validate_content_block_slot(
        fixture,
        content_block,
        "partial_retract",
        &content_block.partial_retract,
        UserInputOperation::Retract,
        ContractCompleteness::Partial,
    )?;
    if content_block.current.content == content_block.correction.content {
        return Err(SemanticFixtureError::invalid(
            "content-block correction must change the normalized content",
        ));
    }
    if content_block.retract.content != content_block.correction.content
        || content_block.partial_retract.content != content_block.correction.content
        || content_block.retract.native_tool_call_or_result_id
            != content_block.correction.native_tool_call_or_result_id
        || content_block.partial_retract.native_tool_call_or_result_id
            != content_block.correction.native_tool_call_or_result_id
        || content_block.current.ordinal != content_block.correction.ordinal
        || content_block.current.ordinal != content_block.retract.ordinal
        || content_block.current.ordinal != content_block.partial_retract.ordinal
    {
        return Err(SemanticFixtureError::invalid(
            "content-block replacement slots must retain the corrected entity value and ordinal",
        ));
    }
    for slot in [
        &fixture.current,
        &fixture.correction,
        &fixture.complete_blocks,
        &fixture.partial_blocks,
        &fixture.retract,
        &fixture.partial,
    ] {
        if !slot
            .ordered_content_block_keys
            .iter()
            .any(|key| key == &content_block.native_content_block_id)
        {
            return Err(SemanticFixtureError::invalid(
                "content-block fixture must belong to every declared parent message snapshot",
            ));
        }
    }
    let refs = [
        &content_block.current.semantic_revision_ref,
        &content_block.correction.semantic_revision_ref,
        &content_block.retract.semantic_revision_ref,
        &content_block.partial_retract.semantic_revision_ref,
    ];
    for (index, left) in refs.iter().enumerate() {
        for right in refs.iter().skip(index + 1) {
            if left == right {
                return Err(SemanticFixtureError::invalid(
                    "content-block revision slots must have distinct semantic identity",
                ));
            }
        }
    }
    Ok(())
}

fn validate_rfc012c_message_fixture(
    fixture: &MessageFixtureWire,
) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION
        || fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "unsupported message fixture contract version",
        ));
    }
    if fixture.family != MESSAGE_FAMILY || fixture.family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(
            "message fixture family must be runtime.message@1",
        ));
    }
    validate_canonical_source_string("adapter_id", &fixture.adapter_id, MAX_ADAPTER_ID_BYTES)?;
    validate_canonical_runtime_text("native_message_id", &fixture.native_message_id)?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        MESSAGE_FAMILY,
        fixture.native_message_id.as_bytes(),
        &fixture.fact_id,
    )?;
    if let Some(content_block) = fixture.content_block.as_ref() {
        validate_content_block_fixture(fixture, content_block)?;
    }
    validate_message_slot(
        fixture,
        "current",
        &fixture.current,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        &["block-a", "block-b"],
    )?;
    validate_message_slot(
        fixture,
        "correction",
        &fixture.correction,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        &["block-a", "block-c"],
    )?;
    validate_message_slot(
        fixture,
        "complete_blocks",
        &fixture.complete_blocks,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        &["block-a"],
    )?;
    validate_message_slot(
        fixture,
        "partial_blocks",
        &fixture.partial_blocks,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
        &["block-a"],
    )?;
    validate_message_slot(
        fixture,
        "retract",
        &fixture.retract,
        UserInputOperation::Retract,
        ContractCompleteness::Complete,
        &["block-a", "block-b"],
    )?;
    validate_message_slot(
        fixture,
        "partial",
        &fixture.partial,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
        &["block-a", "block-b"],
    )?;
    let refs = [
        &fixture.current.semantic_revision_ref,
        &fixture.correction.semantic_revision_ref,
        &fixture.complete_blocks.semantic_revision_ref,
        &fixture.partial_blocks.semantic_revision_ref,
        &fixture.retract.semantic_revision_ref,
        &fixture.partial.semantic_revision_ref,
    ];
    for (index, left) in refs.iter().enumerate() {
        for right in refs.iter().skip(index + 1) {
            if left == right {
                return Err(SemanticFixtureError::invalid(
                    "message revision slots must have distinct semantic identity",
                ));
            }
        }
    }
    Ok(())
}

fn native_marker_kind(value: &NativeRuntimeMarkerValue) -> &'static str {
    match value {
        NativeRuntimeMarkerValue::Compaction { .. } => "compaction",
        NativeRuntimeMarkerValue::Progress { .. } => "progress",
        NativeRuntimeMarkerValue::Queue { .. } => "queue",
    }
}

fn validate_native_marker_slot(
    fixture: &NativeMarkerFixtureWire,
    example: &NativeMarkerExampleWire,
    slot_name: &str,
    slot: &NativeMarkerRevisionSlotWire,
    expected_kind: &str,
    expected_operation: UserInputOperation,
    expected_completeness: ContractCompleteness,
) -> Result<(), SemanticFixtureError> {
    if native_marker_kind(&slot.value) != expected_kind {
        return Err(SemanticFixtureError::invalid(format!(
            "native-marker {slot_name} value kind does not match its fixture example"
        )));
    }
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "native-marker {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != expected_completeness {
        return Err(SemanticFixtureError::invalid(format!(
            "native-marker {slot_name} completeness does not match its fixture slot"
        )));
    }
    let revision = native_marker_revision(fixture, example, slot);
    revision
        .validate()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    verify_fact_revision_identity(
        NATIVE_MARKER_FAMILY,
        &example.fact_id,
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        revision.semantic_revision_key(),
    )
}

fn validate_native_marker_example(
    fixture: &NativeMarkerFixtureWire,
    example: &NativeMarkerExampleWire,
    expected_id: &str,
    expected_kind: &str,
) -> Result<(), SemanticFixtureError> {
    if example.native_marker_id != expected_id {
        return Err(SemanticFixtureError::invalid(format!(
            "native-marker {expected_kind} identity does not match its declared fixture slot"
        )));
    }
    validate_canonical_runtime_text("native_marker_id", &example.native_marker_id)?;
    let current_revision = native_marker_revision(fixture, example, &example.current);
    let stable_native_key = current_revision
        .stable_native_fact_key()
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        NATIVE_MARKER_FAMILY,
        &stable_native_key,
        &example.fact_id,
    )?;
    validate_native_marker_slot(
        fixture,
        example,
        "current",
        &example.current,
        expected_kind,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
    )?;
    validate_native_marker_slot(
        fixture,
        example,
        "correction",
        &example.correction,
        expected_kind,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
    )?;
    validate_native_marker_slot(
        fixture,
        example,
        "retract",
        &example.retract,
        expected_kind,
        UserInputOperation::Retract,
        ContractCompleteness::Complete,
    )?;
    validate_native_marker_slot(
        fixture,
        example,
        "partial",
        &example.partial,
        expected_kind,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
    )?;
    if example.current.value == example.correction.value {
        return Err(SemanticFixtureError::invalid(format!(
            "native-marker {expected_kind} correction must change the typed value"
        )));
    }
    if example.retract.value != example.correction.value
        || example.retract.correlated_native_id != example.correction.correlated_native_id
        || example.retract.quality != example.correction.quality
        || example.retract.provenance != example.correction.provenance
    {
        return Err(SemanticFixtureError::invalid(format!(
            "native-marker {expected_kind} retract must retain the corrected native value"
        )));
    }
    for slot in [&example.correction, &example.retract, &example.partial] {
        let stable = native_marker_revision(fixture, example, slot)
            .stable_native_fact_key()
            .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
        if stable != stable_native_key {
            return Err(SemanticFixtureError::invalid(format!(
                "native-marker {expected_kind} revisions must retain one fact identity"
            )));
        }
    }
    let refs = [
        &example.current.semantic_revision_ref,
        &example.correction.semantic_revision_ref,
        &example.retract.semantic_revision_ref,
        &example.partial.semantic_revision_ref,
    ];
    for (index, left) in refs.iter().enumerate() {
        for right in refs.iter().skip(index + 1) {
            if left == right {
                return Err(SemanticFixtureError::invalid(format!(
                    "native-marker {expected_kind} revision slots must have distinct semantic identity"
                )));
            }
        }
    }
    Ok(())
}

fn validate_rfc012c_native_marker_fixture(
    fixture: &NativeMarkerFixtureWire,
) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION
        || fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "unsupported native-marker fixture contract version",
        ));
    }
    if fixture.family != NATIVE_MARKER_FAMILY || fixture.family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(
            "native-marker fixture family must be runtime.native-marker@1",
        ));
    }
    validate_canonical_source_string("adapter_id", &fixture.adapter_id, MAX_ADAPTER_ID_BYTES)?;
    validate_native_marker_example(fixture, &fixture.compaction, "compaction-1", "compaction")?;
    validate_native_marker_example(fixture, &fixture.progress, "progress-1", "progress")?;
    validate_native_marker_example(fixture, &fixture.queue, "queue-1", "queue")?;
    if fixture.compaction.fact_id == fixture.progress.fact_id
        || fixture.compaction.fact_id == fixture.queue.fact_id
        || fixture.progress.fact_id == fixture.queue.fact_id
    {
        return Err(SemanticFixtureError::invalid(
            "native-marker fixture examples must have distinct fact identities",
        ));
    }
    Ok(())
}

fn validate_task_slot(
    fixture: &TaskFixtureWire,
    slot_name: &str,
    slot: &TaskRevisionSlotWire,
    expected_state: TaskLifecycleState,
    expected_operation: UserInputOperation,
    expected_completeness: ContractCompleteness,
    expected_owned_set: Option<&[&str]>,
) -> Result<(), SemanticFixtureError> {
    if slot.state != expected_state {
        return Err(SemanticFixtureError::invalid(
            "task lifecycle state does not match its fixture slot",
        ));
    }
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "task {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != expected_completeness {
        return Err(SemanticFixtureError::invalid(format!(
            "task {slot_name} completeness does not match its fixture slot"
        )));
    }
    match (expected_owned_set, slot.owned_set.as_ref()) {
        (None, None) => {}
        (Some(expected), Some(actual))
            if actual
                .iter()
                .map(String::as_str)
                .eq(expected.iter().copied()) =>
        {
            for member in actual {
                validate_canonical_runtime_text("owned task id", member)?;
            }
        }
        _ => {
            return Err(SemanticFixtureError::invalid(format!(
                "task {slot_name} owned_set does not match the declared snapshot"
            )));
        }
    }
    verify_fact_revision_identity(
        TASK_FAMILY,
        &fixture.fact_id,
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        task_revision(fixture, &fixture.native_task_id, slot).semantic_revision_key(),
    )
}

fn validate_rfc012c_task_fixture(fixture: &TaskFixtureWire) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION
        || fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "unsupported task fixture contract version",
        ));
    }
    if fixture.family != TASK_FAMILY || fixture.family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(
            "task fixture family must be runtime.task@1",
        ));
    }
    validate_canonical_source_string("adapter_id", &fixture.adapter_id, MAX_ADAPTER_ID_BYTES)?;
    validate_canonical_runtime_text("native_task_id", &fixture.native_task_id)?;
    validate_canonical_runtime_text("peer_native_task_id", &fixture.peer_native_task_id)?;
    validate_canonical_runtime_text("subject", &fixture.subject)?;
    if fixture.native_task_id == fixture.peer_native_task_id {
        return Err(SemanticFixtureError::invalid(
            "task fixture peer must be a distinct task identity",
        ));
    }
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        TASK_FAMILY,
        fixture.native_task_id.as_bytes(),
        &fixture.fact_id,
    )?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        TASK_FAMILY,
        fixture.peer_native_task_id.as_bytes(),
        &fixture.peer_fact_id,
    )?;
    validate_task_slot(
        fixture,
        "created",
        &fixture.created,
        TaskLifecycleState::Created,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        None,
    )?;
    validate_task_slot(
        fixture,
        "updated",
        &fixture.updated,
        TaskLifecycleState::Updated,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        None,
    )?;
    validate_task_slot(
        fixture,
        "completed",
        &fixture.completed,
        TaskLifecycleState::Completed,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        None,
    )?;
    validate_task_slot(
        fixture,
        "retract",
        &fixture.retract,
        TaskLifecycleState::Created,
        UserInputOperation::Retract,
        ContractCompleteness::Complete,
        None,
    )?;
    validate_task_slot(
        fixture,
        "partial",
        &fixture.partial,
        TaskLifecycleState::Created,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
        None,
    )?;
    validate_task_slot(
        fixture,
        "collection_omit",
        &fixture.collection_omit,
        TaskLifecycleState::Created,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        Some(&["fixture-task-2"]),
    )?;
    let refs = [
        &fixture.created.semantic_revision_ref,
        &fixture.updated.semantic_revision_ref,
        &fixture.completed.semantic_revision_ref,
        &fixture.retract.semantic_revision_ref,
        &fixture.partial.semantic_revision_ref,
        &fixture.collection_omit.semantic_revision_ref,
    ];
    for (index, left) in refs.iter().enumerate() {
        for right in refs.iter().skip(index + 1) {
            if left == right {
                return Err(SemanticFixtureError::invalid(
                    "task revision slots must have distinct semantic identity",
                ));
            }
        }
    }
    Ok(())
}

fn validate_plan_slot(
    fixture: &PlanFixtureWire,
    slot_name: &str,
    slot: &PlanRevisionSlotWire,
    expected_operation: UserInputOperation,
    expected_completeness: ContractCompleteness,
    expected_keys: &[&str],
    expected_owned_set: Option<&[&str]>,
) -> Result<(), SemanticFixtureError> {
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "plan {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != expected_completeness {
        return Err(SemanticFixtureError::invalid(format!(
            "plan {slot_name} completeness does not match its fixture slot"
        )));
    }
    if slot.ordered_step_keys.is_empty() || slot.ordered_step_keys.len() > MAX_PLAN_STEPS {
        return Err(SemanticFixtureError::invalid(format!(
            "plan {slot_name} ordered_step_keys must contain 1..={MAX_PLAN_STEPS} keys"
        )));
    }
    if slot.ordered_step_keys != expected_keys {
        return Err(SemanticFixtureError::invalid(format!(
            "plan {slot_name} ordered_step_keys do not match the declared snapshot"
        )));
    }
    let mut seen = BTreeSet::new();
    for key in &slot.ordered_step_keys {
        validate_canonical_runtime_text("plan step key", key)?;
        if !seen.insert(key.as_str()) {
            return Err(SemanticFixtureError::invalid(
                "plan step keys must be unique in one snapshot",
            ));
        }
    }
    match (expected_owned_set, slot.owned_set.as_ref()) {
        (None, None) => {}
        (Some(expected), Some(actual))
            if actual
                .iter()
                .map(String::as_str)
                .eq(expected.iter().copied()) =>
        {
            for member in actual {
                validate_canonical_runtime_text("owned plan id", member)?;
            }
        }
        _ => {
            return Err(SemanticFixtureError::invalid(format!(
                "plan {slot_name} owned_set does not match the declared snapshot"
            )));
        }
    }
    verify_fact_revision_identity(
        PLAN_FAMILY,
        &fixture.fact_id,
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        plan_revision(fixture, &fixture.native_plan_id, slot).semantic_revision_key(),
    )
}

fn validate_rfc012c_plan_fixture(fixture: &PlanFixtureWire) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION
        || fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "unsupported plan fixture contract version",
        ));
    }
    if fixture.family != PLAN_FAMILY || fixture.family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(
            "plan fixture family must be runtime.plan@1",
        ));
    }
    validate_canonical_source_string("adapter_id", &fixture.adapter_id, MAX_ADAPTER_ID_BYTES)?;
    validate_canonical_runtime_text("native_plan_id", &fixture.native_plan_id)?;
    validate_canonical_runtime_text("peer_native_plan_id", &fixture.peer_native_plan_id)?;
    validate_canonical_runtime_text("subject", &fixture.subject)?;
    if fixture.native_plan_id == fixture.peer_native_plan_id {
        return Err(SemanticFixtureError::invalid(
            "plan fixture peer must be a distinct plan identity",
        ));
    }
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        PLAN_FAMILY,
        fixture.native_plan_id.as_bytes(),
        &fixture.fact_id,
    )?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        PLAN_FAMILY,
        fixture.peer_native_plan_id.as_bytes(),
        &fixture.peer_fact_id,
    )?;
    validate_plan_slot(
        fixture,
        "current",
        &fixture.current,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        &["step-a", "step-b"],
        None,
    )?;
    validate_plan_slot(
        fixture,
        "complete_steps",
        &fixture.complete_steps,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        &["step-a"],
        None,
    )?;
    validate_plan_slot(
        fixture,
        "partial_steps",
        &fixture.partial_steps,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
        &["step-a"],
        None,
    )?;
    validate_plan_slot(
        fixture,
        "retract",
        &fixture.retract,
        UserInputOperation::Retract,
        ContractCompleteness::Complete,
        &["step-a", "step-b"],
        None,
    )?;
    validate_plan_slot(
        fixture,
        "partial",
        &fixture.partial,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
        &["step-a", "step-b"],
        None,
    )?;
    validate_plan_slot(
        fixture,
        "collection_omit",
        &fixture.collection_omit,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        &["step-a"],
        Some(&["fixture-plan-2"]),
    )?;
    let refs = [
        &fixture.current.semantic_revision_ref,
        &fixture.complete_steps.semantic_revision_ref,
        &fixture.partial_steps.semantic_revision_ref,
        &fixture.retract.semantic_revision_ref,
        &fixture.partial.semantic_revision_ref,
        &fixture.collection_omit.semantic_revision_ref,
    ];
    for (index, left) in refs.iter().enumerate() {
        for right in refs.iter().skip(index + 1) {
            if left == right {
                return Err(SemanticFixtureError::invalid(
                    "plan revision slots must have distinct semantic identity",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn decode_rfc012c_effective_state_v1(
    json: &str,
) -> Result<EffectiveStateFixtureWire, SemanticFixtureError> {
    let fixture: EffectiveStateFixtureWire = decode_json(json)?;
    validate_rfc012c_effective_state_fixture(&fixture)?;
    Ok(fixture)
}

pub(crate) fn parse_rfc012c_effective_state_v1_json(
    json: &str,
) -> Result<String, SemanticFixtureError> {
    encode_json(&decode_rfc012c_effective_state_v1(json)?)
}

pub(crate) fn decode_rfc012c_interaction_v1(
    json: &str,
) -> Result<InteractionFixtureWire, SemanticFixtureError> {
    let fixture: InteractionFixtureWire = decode_json(json)?;
    validate_rfc012c_interaction_fixture(&fixture)?;
    Ok(fixture)
}

pub(crate) fn parse_rfc012c_interaction_v1_json(
    json: &str,
) -> Result<String, SemanticFixtureError> {
    encode_json(&decode_rfc012c_interaction_v1(json)?)
}

pub(crate) fn decode_rfc012c_message_v1(
    json: &str,
) -> Result<MessageFixtureWire, SemanticFixtureError> {
    let fixture: MessageFixtureWire = decode_json(json)?;
    validate_rfc012c_message_fixture(&fixture)?;
    Ok(fixture)
}

pub(crate) fn parse_rfc012c_message_v1_json(json: &str) -> Result<String, SemanticFixtureError> {
    encode_json(&decode_rfc012c_message_v1(json)?)
}

fn required_fixture_object<'a>(
    value: &'a serde_json::Value,
    label: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, SemanticFixtureError> {
    value
        .as_object()
        .ok_or_else(|| SemanticFixtureError::invalid(format!("{label} must be an object")))
}

fn required_fixture_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    label: &str,
) -> Result<&'a serde_json::Value, SemanticFixtureError> {
    object.get(field).ok_or_else(|| {
        SemanticFixtureError::invalid(format!("{label} is missing required field {field}"))
    })
}

fn preflight_native_marker_required_nullable_fields(
    value: &serde_json::Value,
) -> Result<(), SemanticFixtureError> {
    let fixture = required_fixture_object(value, "native-marker fixture")?;
    for (example_name, value_fields) in [
        ("compaction", &["trigger", "pre_tokens"][..]),
        ("progress", &["completed", "total", "detail_digest"][..]),
        ("queue", &["depth", "item_digest"][..]),
    ] {
        let example = required_fixture_object(
            required_fixture_field(fixture, example_name, "native-marker fixture")?,
            "native-marker example",
        )?;
        for slot_name in ["current", "correction", "retract", "partial"] {
            let slot = required_fixture_object(
                required_fixture_field(example, slot_name, "native-marker example")?,
                "native-marker revision slot",
            )?;
            required_fixture_field(slot, "correlated_native_id", "native-marker revision slot")?;
            required_fixture_field(slot, "effective_at", "native-marker revision slot")?;
            let marker_value = required_fixture_object(
                required_fixture_field(slot, "value", "native-marker revision slot")?,
                "native-marker value",
            )?;
            for field in value_fields {
                required_fixture_field(marker_value, field, "native-marker value")?;
            }
        }
    }
    Ok(())
}

pub(crate) fn decode_rfc012c_native_marker_v1(
    json: &str,
) -> Result<NativeMarkerFixtureWire, SemanticFixtureError> {
    let value = preflight_json(json)?;
    preflight_native_marker_required_nullable_fields(&value)?;
    let fixture: NativeMarkerFixtureWire = serde_json::from_value(value)
        .map_err(|error| SemanticFixtureError::invalid(error.to_string()))?;
    validate_rfc012c_native_marker_fixture(&fixture)?;
    Ok(fixture)
}

pub(crate) fn parse_rfc012c_native_marker_v1_json(
    json: &str,
) -> Result<String, SemanticFixtureError> {
    encode_json(&decode_rfc012c_native_marker_v1(json)?)
}

pub(crate) fn decode_rfc012c_task_v1(json: &str) -> Result<TaskFixtureWire, SemanticFixtureError> {
    let fixture: TaskFixtureWire = decode_json(json)?;
    validate_rfc012c_task_fixture(&fixture)?;
    Ok(fixture)
}

pub(crate) fn parse_rfc012c_task_v1_json(json: &str) -> Result<String, SemanticFixtureError> {
    encode_json(&decode_rfc012c_task_v1(json)?)
}

pub(crate) fn decode_rfc012c_plan_v1(json: &str) -> Result<PlanFixtureWire, SemanticFixtureError> {
    let fixture: PlanFixtureWire = decode_json(json)?;
    validate_rfc012c_plan_fixture(&fixture)?;
    Ok(fixture)
}

pub(crate) fn parse_rfc012c_plan_v1_json(json: &str) -> Result<String, SemanticFixtureError> {
    encode_json(&decode_rfc012c_plan_v1(json)?)
}

fn tool_slot_native_id<'a>(fixture: &'a ToolFixtureWire, slot: &ToolRevisionSlotWire) -> &'a str {
    match slot.kind {
        ToolRevisionKind::Call => fixture.native_call_id.as_str(),
        ToolRevisionKind::Result => fixture.native_result_id.as_str(),
    }
}

fn validate_tool_slot(
    fixture: &ToolFixtureWire,
    slot_name: &str,
    slot: &ToolRevisionSlotWire,
    expected_kind: ToolRevisionKind,
    expected_operation: UserInputOperation,
    expected_completeness: ContractCompleteness,
    expected_correlated: Option<&str>,
) -> Result<(), SemanticFixtureError> {
    if slot.kind != expected_kind {
        return Err(SemanticFixtureError::invalid(format!(
            "tool {slot_name} kind does not match its fixture slot"
        )));
    }
    if slot.operation != expected_operation {
        return Err(SemanticFixtureError::invalid(format!(
            "tool {slot_name} operation does not match its fixture slot"
        )));
    }
    if slot.completeness != expected_completeness {
        return Err(SemanticFixtureError::invalid(format!(
            "tool {slot_name} completeness does not match its fixture slot"
        )));
    }
    match (expected_correlated, slot.correlated_native_id.as_deref()) {
        (None, None) => {}
        (Some(expected), Some(actual)) if actual == expected => {
            validate_canonical_runtime_text("correlated_native_id", actual)?;
        }
        _ => {
            return Err(SemanticFixtureError::invalid(format!(
                "tool {slot_name} correlation does not match the declared snapshot"
            )));
        }
    }
    verify_fact_revision_identity(
        TOOL_FAMILY,
        match slot.kind {
            ToolRevisionKind::Call => &fixture.fact_id,
            ToolRevisionKind::Result => &fixture.result_fact_id,
        },
        &slot.semantic_revision_key_hex,
        &slot.semantic_revision_ref,
        tool_revision(fixture, slot).semantic_revision_key(),
    )
}

fn validate_rfc012c_tool_fixture(fixture: &ToolFixtureWire) -> Result<(), SemanticFixtureError> {
    if fixture.fixture_contract_version != RFC012C_FIXTURE_CONTRACT_VERSION
        || fixture.runtime_semantic_contract_version != RUNTIME_SEMANTIC_CONTRACT_VERSION
    {
        return Err(SemanticFixtureError::invalid(
            "unsupported tool fixture contract version",
        ));
    }
    if fixture.family != TOOL_FAMILY || fixture.family_version != FAMILY_VERSION {
        return Err(SemanticFixtureError::invalid(
            "tool fixture family must be runtime.tool@1",
        ));
    }
    validate_canonical_source_string("adapter_id", &fixture.adapter_id, MAX_ADAPTER_ID_BYTES)?;
    validate_canonical_runtime_text("native_call_id", &fixture.native_call_id)?;
    validate_canonical_runtime_text("native_result_id", &fixture.native_result_id)?;
    validate_canonical_runtime_text("tool_name", &fixture.tool_name)?;
    if fixture.native_call_id == fixture.native_result_id {
        return Err(SemanticFixtureError::invalid(
            "tool fixture call and result must be distinct identities",
        ));
    }
    if fixture.tool_name != "read" {
        return Err(SemanticFixtureError::invalid(
            "tool fixture tool_name must be the declared bounded name",
        ));
    }
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        TOOL_FAMILY,
        fixture.native_call_id.as_bytes(),
        &fixture.fact_id,
    )?;
    verify_committed_fact_id(
        &fixture.adapter_id,
        &fixture.source_instance_key,
        TOOL_FAMILY,
        fixture.native_result_id.as_bytes(),
        &fixture.result_fact_id,
    )?;
    validate_tool_slot(
        fixture,
        "call",
        &fixture.call,
        ToolRevisionKind::Call,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        None,
    )?;
    validate_tool_slot(
        fixture,
        "unmatched_result",
        &fixture.unmatched_result,
        ToolRevisionKind::Result,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        None,
    )?;
    validate_tool_slot(
        fixture,
        "correlated_call",
        &fixture.correlated_call,
        ToolRevisionKind::Call,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        Some(&fixture.native_result_id),
    )?;
    validate_tool_slot(
        fixture,
        "correlated_result",
        &fixture.correlated_result,
        ToolRevisionKind::Result,
        UserInputOperation::Upsert,
        ContractCompleteness::Complete,
        Some(&fixture.native_call_id),
    )?;
    validate_tool_slot(
        fixture,
        "retract",
        &fixture.retract,
        ToolRevisionKind::Call,
        UserInputOperation::Retract,
        ContractCompleteness::Complete,
        None,
    )?;
    validate_tool_slot(
        fixture,
        "partial",
        &fixture.partial,
        ToolRevisionKind::Call,
        UserInputOperation::Upsert,
        ContractCompleteness::Partial,
        None,
    )?;
    let refs = [
        &fixture.call.semantic_revision_ref,
        &fixture.unmatched_result.semantic_revision_ref,
        &fixture.correlated_call.semantic_revision_ref,
        &fixture.correlated_result.semantic_revision_ref,
        &fixture.retract.semantic_revision_ref,
        &fixture.partial.semantic_revision_ref,
    ];
    for (index, left) in refs.iter().enumerate() {
        for right in refs.iter().skip(index + 1) {
            if left == right {
                return Err(SemanticFixtureError::invalid(
                    "tool revision slots must have distinct semantic identity",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn decode_rfc012c_tool_v1(json: &str) -> Result<ToolFixtureWire, SemanticFixtureError> {
    let fixture: ToolFixtureWire = decode_json(json)?;
    validate_rfc012c_tool_fixture(&fixture)?;
    Ok(fixture)
}

pub(crate) fn parse_rfc012c_tool_v1_json(json: &str) -> Result<String, SemanticFixtureError> {
    encode_json(&decode_rfc012c_tool_v1(json)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{NativeCompactionPhase, NativeProgressState, NativeQueueOperation};

    const RFC012A_FIXTURE: &str = include_str!("../fixtures/contracts/rfc012a-v1.json");
    const RFC012C_FIXTURE: &str = include_str!("../fixtures/contracts/rfc012c-runtime-v1.json");
    const RFC012C_EFFECTIVE_STATE_FIXTURE: &str =
        include_str!("../fixtures/contracts/rfc012c-effective-state-v1.json");
    const RFC012C_EFFECTIVE_STATE_EFFORT_FIXTURE: &str =
        include_str!("../fixtures/contracts/rfc012c-effective-state-effort-v1.json");
    const RFC012C_EFFECTIVE_STATE_SESSION_MODE_FIXTURE: &str =
        include_str!("../fixtures/contracts/rfc012c-effective-state-session-mode-v1.json");
    const RFC012C_EFFECTIVE_STATE_PERMISSION_MODE_FIXTURE: &str =
        include_str!("../fixtures/contracts/rfc012c-effective-state-permission-mode-v1.json");
    const RFC012C_INTERACTION_FIXTURE: &str =
        include_str!("../fixtures/contracts/rfc012c-interaction-v1.json");
    const RFC012C_MESSAGE_FIXTURE: &str =
        include_str!("../fixtures/contracts/rfc012c-message-v1.json");
    const RFC012C_NATIVE_MARKER_FIXTURE: &str =
        include_str!("../fixtures/contracts/rfc012c-native-marker-v1.json");
    const RFC012C_TASK_FIXTURE: &str = include_str!("../fixtures/contracts/rfc012c-task-v1.json");
    const RFC012C_PLAN_FIXTURE: &str = include_str!("../fixtures/contracts/rfc012c-plan-v1.json");
    const RFC012C_TOOL_FIXTURE: &str = include_str!("../fixtures/contracts/rfc012c-tool-v1.json");

    fn assert_privacy_safe(json: &str) {
        assert!(!json.contains("/Users/"));
        assert!(!json.contains("~/"));
        assert!(!json.contains("\\\\Users\\\\"));
        assert!(!json.contains("C:\\\\"));
        assert!(!json.contains(".db"));
        assert!(!json.contains("sqlite"));
    }

    fn committed_native_marker_slot(
        fact_id: &CanonicalFactId,
        revision: NativeRuntimeMarkerRevisionFact,
    ) -> NativeMarkerRevisionSlotWire {
        let semantic_revision_key = revision.semantic_revision_key().unwrap();
        NativeMarkerRevisionSlotWire {
            correlated_native_id: revision.correlated_native_id,
            value: revision.value,
            quality: revision.quality,
            effective_at: revision.effective_at,
            provenance: revision.provenance,
            completeness: revision.completeness,
            operation: revision.operation,
            semantic_revision_key_hex: hex_digest(&semantic_revision_key),
            semantic_revision_ref: SemanticRevisionRef::new(
                FactRevisionId::derive(fact_id, 1, &semantic_revision_key).unwrap(),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn committed_native_marker_example(
        adapter_id: &str,
        source_instance_key: &CanonicalSourceInstanceKey,
        session: CanonicalEntityKey,
        actor_run: CanonicalEntityKey,
        native_marker_id: &str,
        correlated_native_id: Option<&str>,
        current_value: NativeRuntimeMarkerValue,
        correction_value: NativeRuntimeMarkerValue,
        partial_value: NativeRuntimeMarkerValue,
        quality: QualifiedValueQuality,
        native_field: &str,
        base_time: i64,
    ) -> NativeMarkerExampleWire {
        let revision = |value: NativeRuntimeMarkerValue,
                        completeness: ContractCompleteness,
                        operation: UserInputOperation,
                        effective_at: i64| NativeRuntimeMarkerRevisionFact {
            session,
            actor_run,
            native_marker_id: native_marker_id.to_owned(),
            correlated_native_id: correlated_native_id.map(str::to_owned),
            value,
            quality,
            effective_at: Some(effective_at),
            provenance: NativeRuntimeMarkerProvenance {
                native_field: native_field.to_owned(),
                normalization_contract_version: 1,
            },
            completeness,
            operation,
        };
        let current_revision = revision(
            current_value,
            ContractCompleteness::Complete,
            UserInputOperation::Upsert,
            base_time,
        );
        let stable_native_key = current_revision.stable_native_fact_key().unwrap();
        let fact_id = CanonicalFactId::native(
            adapter_id,
            source_instance_key,
            NATIVE_MARKER_FAMILY,
            &stable_native_key,
        )
        .unwrap();
        let correction_revision = revision(
            correction_value.clone(),
            ContractCompleteness::Complete,
            UserInputOperation::Upsert,
            base_time + 1,
        );
        let retract_revision = revision(
            correction_value,
            ContractCompleteness::Complete,
            UserInputOperation::Retract,
            base_time + 2,
        );
        let partial_revision = revision(
            partial_value,
            ContractCompleteness::Partial,
            UserInputOperation::Upsert,
            base_time + 3,
        );
        NativeMarkerExampleWire {
            native_marker_id: native_marker_id.to_owned(),
            fact_id,
            current: committed_native_marker_slot(&fact_id, current_revision),
            correction: committed_native_marker_slot(&fact_id, correction_revision),
            retract: committed_native_marker_slot(&fact_id, retract_revision),
            partial: committed_native_marker_slot(&fact_id, partial_revision),
        }
    }

    #[test]
    fn rfc012c_native_marker_fixture_is_rust_generated_and_strict() {
        let context = decode_rfc012c_effective_state_v1(RFC012C_EFFECTIVE_STATE_FIXTURE).unwrap();
        let fixture = NativeMarkerFixtureWire {
            adapter_id: context.adapter_id,
            actor_run: context.actor_run,
            family: NATIVE_MARKER_FAMILY.to_owned(),
            family_version: FAMILY_VERSION,
            fixture_contract_version: RFC012C_FIXTURE_CONTRACT_VERSION,
            runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
            session: context.session,
            source_instance_key: context.source_instance_key,
            source_record_id: context.source_record_id,
            compaction: committed_native_marker_example(
                "fixture-adapter",
                &context.source_instance_key,
                context.session,
                context.actor_run,
                "compaction-1",
                None,
                NativeRuntimeMarkerValue::Compaction {
                    phase: NativeCompactionPhase::Started,
                    trigger: Some("auto".to_owned()),
                    pre_tokens: Some(1_000),
                },
                NativeRuntimeMarkerValue::Compaction {
                    phase: NativeCompactionPhase::Completed,
                    trigger: Some("auto".to_owned()),
                    pre_tokens: Some(900),
                },
                NativeRuntimeMarkerValue::Compaction {
                    phase: NativeCompactionPhase::Boundary,
                    trigger: Some("manual".to_owned()),
                    pre_tokens: Some(950),
                },
                QualifiedValueQuality::Exact,
                "runtime.compaction",
                1_776_211_200_000,
            ),
            progress: committed_native_marker_example(
                "fixture-adapter",
                &context.source_instance_key,
                context.session,
                context.actor_run,
                "progress-1",
                Some("tool-1"),
                NativeRuntimeMarkerValue::Progress {
                    state: NativeProgressState::Pending,
                    completed: Some(0),
                    total: Some(2),
                    detail_digest: Some([7; 32]),
                },
                NativeRuntimeMarkerValue::Progress {
                    state: NativeProgressState::Completed,
                    completed: Some(2),
                    total: Some(2),
                    detail_digest: Some([8; 32]),
                },
                NativeRuntimeMarkerValue::Progress {
                    state: NativeProgressState::Active,
                    completed: Some(1),
                    total: Some(2),
                    detail_digest: Some([9; 32]),
                },
                QualifiedValueQuality::NativeClaimed,
                "progress.data",
                1_776_211_200_100,
            ),
            queue: committed_native_marker_example(
                "fixture-adapter",
                &context.source_instance_key,
                context.session,
                context.actor_run,
                "queue-1",
                Some("tool-1"),
                NativeRuntimeMarkerValue::Queue {
                    operation: NativeQueueOperation::Enqueue,
                    depth: Some(1),
                    item_digest: Some([10; 32]),
                },
                NativeRuntimeMarkerValue::Queue {
                    operation: NativeQueueOperation::Dequeue,
                    depth: Some(0),
                    item_digest: Some([10; 32]),
                },
                NativeRuntimeMarkerValue::Queue {
                    operation: NativeQueueOperation::Enqueue,
                    depth: Some(2),
                    item_digest: Some([11; 32]),
                },
                QualifiedValueQuality::Exact,
                "queue.operation",
                1_776_211_200_200,
            ),
        };
        let committed = decode_rfc012c_native_marker_v1(RFC012C_NATIVE_MARKER_FIXTURE).unwrap();
        assert_eq!(committed, fixture);
        let parsed = parse_rfc012c_native_marker_v1_json(RFC012C_NATIVE_MARKER_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&parsed).unwrap(),
            serde_json::from_str::<serde_json::Value>(RFC012C_NATIVE_MARKER_FIXTURE).unwrap()
        );
    }

    #[test]
    fn rfc012c_native_marker_fixture_rejects_drift_and_non_native_claims() {
        let fixture =
            || serde_json::from_str::<serde_json::Value>(RFC012C_NATIVE_MARKER_FIXTURE).unwrap();

        let mut semantic_drift = fixture();
        semantic_drift["progress"]["current"]["value"]["state"] = serde_json::json!("completed");
        assert!(parse_rfc012c_native_marker_v1_json(&semantic_drift.to_string()).is_err());

        let mut host_assessment = fixture();
        host_assessment["progress"]["current"]["quality"] = serde_json::json!("derived");
        assert!(parse_rfc012c_native_marker_v1_json(&host_assessment.to_string()).is_err());

        let mut impossible_progress = fixture();
        impossible_progress["progress"]["current"]["value"]["completed"] = serde_json::json!(3);
        assert!(parse_rfc012c_native_marker_v1_json(&impossible_progress.to_string()).is_err());

        let mut zero_digest = fixture();
        zero_digest["queue"]["current"]["value"]["item_digest"] = serde_json::json!(vec![0; 32]);
        assert!(parse_rfc012c_native_marker_v1_json(&zero_digest.to_string()).is_err());

        let mut wrong_kind = fixture();
        wrong_kind["compaction"]["current"]["value"] =
            wrong_kind["progress"]["current"]["value"].clone();
        assert!(parse_rfc012c_native_marker_v1_json(&wrong_kind.to_string()).is_err());

        let mut identity_drift = fixture();
        identity_drift["queue"]["fact_id"] = identity_drift["progress"]["fact_id"].clone();
        assert!(parse_rfc012c_native_marker_v1_json(&identity_drift.to_string()).is_err());

        let mut unknown = fixture();
        unknown["queue"]["current"]["value"]["future"] = serde_json::json!(true);
        assert!(parse_rfc012c_native_marker_v1_json(&unknown.to_string()).is_err());

        let mut missing_nullable = fixture();
        missing_nullable["compaction"]["current"]
            .as_object_mut()
            .unwrap()
            .remove("correlated_native_id");
        assert!(parse_rfc012c_native_marker_v1_json(&missing_nullable.to_string()).is_err());

        let mut missing_value_nullable = fixture();
        missing_value_nullable["compaction"]["current"]["value"]
            .as_object_mut()
            .unwrap()
            .remove("pre_tokens");
        assert!(parse_rfc012c_native_marker_v1_json(&missing_value_nullable.to_string()).is_err());

        assert!(
            parse_rfc012c_native_marker_v1_json(&RFC012C_NATIVE_MARKER_FIXTURE.replacen(
                "\"completed\": 0",
                "\"completed\": 0.0",
                1,
            ),)
            .is_err()
        );
    }

    #[test]
    fn rfc012a_fixture_parses_and_preserves_identities() {
        let parsed = parse_rfc012a_v1_json(RFC012A_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value = serde_json::from_str(RFC012A_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: Rfc012aFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(
            fixture.external_entity_ref.entity_key,
            fixture.native_identity_claim.entity_ref.entity_key
        );
        assert_eq!(fixture.qualified_known_zero.value, Some(0));
        assert_eq!(
            compare_coverage(&fixture.coverage.dominant, &fixture.coverage.baseline).unwrap(),
            CoverageComparison::Dominates
        );
    }

    #[test]
    fn rfc012c_fixture_parses_and_preserves_identities() {
        let parsed = parse_rfc012c_runtime_v1_json(RFC012C_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value = serde_json::from_str(RFC012C_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: RuntimeContractFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(
            fixture.actors.child.revision.parent_actor_run,
            Some(fixture.actors.root.revision.actor_run)
        );
        assert_eq!(
            fixture.usage.response_revisions.a.semantic_revision_ref,
            fixture
                .usage
                .response_revisions
                .a_repeat
                .semantic_revision_ref
        );
        assert_ne!(
            fixture.usage.response_revisions.a.semantic_revision_ref,
            fixture.usage.response_revisions.b.semantic_revision_ref
        );
    }

    #[test]
    fn unknown_fields_and_incompatible_majors_are_rejected() {
        let mut rfc012a: serde_json::Value = serde_json::from_str(RFC012A_FIXTURE).unwrap();
        rfc012a["future"] = serde_json::json!(true);
        assert!(parse_rfc012a_v1_json(&rfc012a.to_string()).is_err());

        let mut qualified = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        qualified["qualified_known_zero"]["future"] = serde_json::json!(true);
        assert!(parse_rfc012a_v1_json(&qualified.to_string()).is_err());

        let mut coverage = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        coverage["coverage"]["baseline"]["points"][0]["future"] = serde_json::json!(true);
        assert!(parse_rfc012a_v1_json(&coverage.to_string()).is_err());

        let mut major = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        major["external_entity_ref"]["external_entity_reference_version"] = serde_json::json!(2);
        assert!(parse_rfc012a_v1_json(&major.to_string()).is_err());

        let mut rfc012c: serde_json::Value = serde_json::from_str(RFC012C_FIXTURE).unwrap();
        rfc012c["actors"]["root"]["revision"]["extra_future_field"] = serde_json::json!("ignored");
        assert!(parse_rfc012c_runtime_v1_json(&rfc012c.to_string()).is_err());

        let mut usage = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        usage["usage"]["native_message"]["revision"]["buckets"]["input_tokens"]["future"] =
            serde_json::json!(true);
        assert!(parse_rfc012c_runtime_v1_json(&usage.to_string()).is_err());

        let mut fixture_major = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        fixture_major["fixture_contract_version"] = serde_json::json!(2);
        assert!(parse_rfc012c_runtime_v1_json(&fixture_major.to_string()).is_err());
    }

    #[test]
    fn empty_oversized_and_unbounded_json_graphs_are_rejected() {
        assert!(parse_rfc012a_v1_json("").is_err());
        assert!(parse_rfc012c_runtime_v1_json("").is_err());
        let oversized = "x".repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1);
        assert!(parse_rfc012a_v1_json(&oversized).is_err());
        assert!(parse_rfc012c_runtime_v1_json(&oversized).is_err());

        let mut too_deep = serde_json::json!(0);
        for _ in 0..MAX_SEMANTIC_FIXTURE_DEPTH {
            too_deep = serde_json::json!({ "child": too_deep });
        }
        assert!(parse_rfc012a_v1_json(&too_deep.to_string()).is_err());
        assert!(parse_rfc012c_runtime_v1_json(&too_deep.to_string()).is_err());

        let too_wide = serde_json::json!((0..=MAX_SEMANTIC_FIXTURE_NODES).collect::<Vec<_>>());
        assert!(parse_rfc012a_v1_json(&too_wide.to_string()).is_err());
        assert!(parse_rfc012c_runtime_v1_json(&too_wide.to_string()).is_err());
    }

    #[test]
    fn qualified_explicit_nulls_are_rejected_on_the_rfc012a_fixture() {
        let mut effective_at = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        effective_at["qualified_known_zero"]["effective_at"] = serde_json::Value::Null;
        assert!(parse_rfc012a_v1_json(&effective_at.to_string()).is_err());

        let mut unknown_reason =
            serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        unknown_reason["qualified_known_zero"]["unknown_reason"] = serde_json::Value::Null;
        assert!(parse_rfc012a_v1_json(&unknown_reason.to_string()).is_err());
    }

    #[test]
    fn rfc012c_source_strings_must_be_canonical_and_non_empty() {
        let mut adapter = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        adapter["source"]["adapter_id"] = serde_json::json!("");
        assert!(parse_rfc012c_runtime_v1_json(&adapter.to_string()).is_err());

        let mut padded = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        padded["source"]["adapter_id"] = serde_json::json!(" fixture-adapter");
        assert!(parse_rfc012c_runtime_v1_json(&padded.to_string()).is_err());

        let mut session = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        session["source"]["session"]["native_session_id"] = serde_json::json!("");
        assert!(parse_rfc012c_runtime_v1_json(&session.to_string()).is_err());
    }

    #[test]
    fn rfc012c_closed_family_workflow_and_usage_actor_relations_are_enforced() {
        let mut reordered = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        let families = reordered["families"].as_array_mut().unwrap();
        families.swap(0, 2);
        assert!(parse_rfc012c_runtime_v1_json(&reordered.to_string()).is_err());

        let mut extra = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        extra["families"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "family": "runtime.session",
                "version": 1
            }));
        assert!(parse_rfc012c_runtime_v1_json(&extra.to_string()).is_err());

        let mut fact_id = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        fact_id["affiliations"]["child_workflow_removed"]["fact_id"] =
            fact_id["affiliations"]["child_team_present"]["fact_id"].clone();
        assert!(parse_rfc012c_runtime_v1_json(&fact_id.to_string()).is_err());

        let mut reused = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        reused["affiliations"]["child_workflow_removed"]["semantic_revision_ref"] =
            reused["affiliations"]["child_workflow_present"]["semantic_revision_ref"].clone();
        assert!(parse_rfc012c_runtime_v1_json(&reused.to_string()).is_err());

        let mut actor = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        actor["usage"]["native_message"]["revision"]["actor_run"] =
            actor["affiliations"]["child_team_present"]["revision"]["target"].clone();
        assert!(parse_rfc012c_runtime_v1_json(&actor.to_string()).is_err());

        let mut source_record = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        source_record["actors"]["root"]["source_record_id"] =
            source_record["source"]["session"]["entity_key"].clone();
        assert!(parse_rfc012c_runtime_v1_json(&source_record.to_string()).is_err());

        assert!(parse_rfc012c_runtime_v1_json(&RFC012C_FIXTURE.replace(
            "\"adapter_id\": \"fixture-adapter\"",
            "\"adapter_id\": \"\\u0085fixture-adapter\""
        ))
        .is_err());
        assert!(parse_rfc012c_runtime_v1_json(&RFC012C_FIXTURE.replace(
            "\"adapter_id\": \"fixture-adapter\"",
            "\"adapter_id\": \"\\ufefffixture-adapter\""
        ))
        .is_ok());
        assert!(parse_rfc012a_v1_json(&RFC012A_FIXTURE.replace(
            "\"authority\": \"native-response\"",
            "\"authority\": \"\\u0085native-response\""
        ))
        .is_err());
        assert!(parse_rfc012a_v1_json(&RFC012A_FIXTURE.replace(
            "\"authority\": \"native-response\"",
            "\"authority\": \"\\ufeffnative-response\""
        ))
        .is_ok());
    }

    #[test]
    fn rfc012a_empty_authority_and_oversized_native_identity_are_rejected() {
        let mut authority = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        authority["qualified_known_zero"]["authority"] = serde_json::json!("");
        assert!(parse_rfc012a_v1_json(&authority.to_string()).is_err());

        let mut padded = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        padded["qualified_unknown"]["authority"] = serde_json::json!(" native-response");
        assert!(parse_rfc012a_v1_json(&padded.to_string()).is_err());

        let mut identity = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        identity["native_identity_claim"]["identity"]["value"]["native_id"] =
            serde_json::json!("a".repeat(257));
        assert!(parse_rfc012a_v1_json(&identity.to_string()).is_err());

        let mut exact_identity =
            serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        exact_identity["native_identity_claim"]["identity"]["value"]["native_id"] =
            serde_json::json!("a".repeat(256));
        assert!(parse_rfc012a_v1_json(&exact_identity.to_string()).is_ok());

        let mut exact_authority =
            serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        exact_authority["qualified_known_zero"]["authority"] = serde_json::json!("a".repeat(256));
        assert!(parse_rfc012a_v1_json(&exact_authority.to_string()).is_ok());
        exact_authority["qualified_known_zero"]["authority"] = serde_json::json!("a".repeat(257));
        assert!(parse_rfc012a_v1_json(&exact_authority.to_string()).is_err());
        exact_authority["qualified_known_zero"]["authority"] =
            serde_json::json!("a".repeat(200_000));
        assert!(parse_rfc012a_v1_json(&exact_authority.to_string()).is_err());

        let mut exact_adapter = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        exact_adapter["source"]["adapter_id"] = serde_json::json!("a".repeat(128));
        assert!(parse_rfc012c_runtime_v1_json(&exact_adapter.to_string()).is_ok());
        exact_adapter["source"]["adapter_id"] = serde_json::json!("a".repeat(129));
        assert!(parse_rfc012c_runtime_v1_json(&exact_adapter.to_string()).is_err());
        exact_adapter["source"]["adapter_id"] = serde_json::json!("a".repeat(200_000));
        assert!(parse_rfc012c_runtime_v1_json(&exact_adapter.to_string()).is_err());

        let mut exact_session = serde_json::from_str::<serde_json::Value>(RFC012C_FIXTURE).unwrap();
        exact_session["source"]["session"]["native_session_id"] =
            serde_json::json!("a".repeat(8 * 1024));
        assert!(parse_rfc012c_runtime_v1_json(&exact_session.to_string()).is_ok());
        exact_session["source"]["session"]["native_session_id"] =
            serde_json::json!("a".repeat(8 * 1024 + 1));
        assert!(parse_rfc012c_runtime_v1_json(&exact_session.to_string()).is_err());
        exact_session["source"]["session"]["native_session_id"] =
            serde_json::json!("a".repeat(200_000));
        assert!(parse_rfc012c_runtime_v1_json(&exact_session.to_string()).is_err());
    }

    #[test]
    fn rfc012a_semantic_revision_ref_binds_identity_and_known_zero_provenance() {
        let mut top_level = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        top_level["semantic_revision_ref"]["fact_revision_id"] =
            top_level["canonical_source_instance_key"].clone();
        assert!(parse_rfc012a_v1_json(&top_level.to_string()).is_err());

        let mut identity = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        identity["native_identity_claim"]["identity"]["provenance"][0]["fact_revision_id"] =
            identity["canonical_source_instance_key"].clone();
        assert!(parse_rfc012a_v1_json(&identity.to_string()).is_err());

        let mut known_zero = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        known_zero["qualified_known_zero"]["provenance"][0]["fact_revision_id"] =
            known_zero["canonical_source_instance_key"].clone();
        assert!(parse_rfc012a_v1_json(&known_zero.to_string()).is_err());

        let mut unknown = serde_json::from_str::<serde_json::Value>(RFC012A_FIXTURE).unwrap();
        unknown["qualified_unknown"]["provenance"] = serde_json::json!([{
            "fact_revision_id": unknown["semantic_revision_ref"]["fact_revision_id"],
            "semantic_reference_contract_version": 1
        }]);
        assert!(parse_rfc012a_v1_json(&unknown.to_string()).is_err());
    }

    #[test]
    fn rfc012a_and_rfc012c_reject_noncanonical_integer_lexemes() {
        assert!(parse_rfc012a_v1_json(&RFC012A_FIXTURE.replace(
            "\"effective_at\": 1776211200000",
            "\"effective_at\": 1776211200000.0"
        ))
        .is_err());
        assert!(parse_rfc012c_runtime_v1_json(&RFC012C_FIXTURE.replacen(
            "\"value\": 0",
            "\"value\": 0.0",
            1
        ))
        .is_err());
        assert!(parse_rfc012c_runtime_v1_json(&RFC012C_FIXTURE.replacen(
            "\"value\": 0",
            "\"value\": -0",
            1
        ))
        .is_err());
        assert!(parse_rfc012c_runtime_v1_json(&RFC012C_FIXTURE.replacen(
            "\"value\": 0",
            "\"value\": 0e0",
            1
        ))
        .is_err());
        assert!(parse_rfc012a_v1_json(&RFC012A_FIXTURE.replace(
            "\"authority\": \"native-response\"",
            "\"authority\": \"\\ud800\""
        ))
        .is_err());
    }

    #[test]
    fn semantic_fixture_envelope_accepts_exact_bounds_and_rejects_one_over() {
        let mut exact_depth = serde_json::json!(0);
        for _ in 0..(MAX_SEMANTIC_FIXTURE_DEPTH - 1) {
            exact_depth = serde_json::json!({ "child": exact_depth });
        }
        assert!(preflight_json(&exact_depth.to_string()).is_ok());
        let mut too_deep = exact_depth;
        too_deep = serde_json::json!({ "child": too_deep });
        assert!(preflight_json(&too_deep.to_string()).is_err());

        let exact_nodes = serde_json::json!(
            (0..MAX_SEMANTIC_FIXTURE_NODES.saturating_sub(1)).collect::<Vec<_>>()
        );
        assert!(preflight_json(&exact_nodes.to_string()).is_ok());
        let too_wide = serde_json::json!((0..MAX_SEMANTIC_FIXTURE_NODES).collect::<Vec<_>>());
        assert!(preflight_json(&too_wide.to_string()).is_err());

        let mut padded = RFC012A_FIXTURE.to_string();
        padded.push_str(&" ".repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES - padded.len()));
        assert_eq!(padded.len(), MAX_SEMANTIC_FIXTURE_JSON_BYTES);
        assert!(parse_rfc012a_v1_json(&padded).is_ok());
        padded.push(' ');
        assert!(parse_rfc012a_v1_json(&padded).is_err());
        padded.push_str(&" ".repeat(1_000_000));
        assert!(parse_rfc012a_v1_json(&padded).is_err());
    }

    #[test]
    fn encoded_response_key_rejects_overlong_base64_before_decode() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        use serde::Deserialize;

        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "strict_standard_base64::deserialize")]
            value: Vec<u8>,
        }

        let exact = STANDARD.encode(vec![b'a'; MAX_USAGE_RESPONSE_KEY_BYTES]);
        assert_eq!(exact.len(), MAX_USAGE_RESPONSE_KEY_BASE64_CHARS);
        let parsed: Wrapper = serde_json::from_str(&format!(r#"{{"value":"{exact}"}}"#)).unwrap();
        assert_eq!(parsed.value.len(), MAX_USAGE_RESPONSE_KEY_BYTES);

        let over = "A".repeat(MAX_USAGE_RESPONSE_KEY_BASE64_CHARS + 1);
        assert!(serde_json::from_str::<Wrapper>(&format!(r#"{{"value":"{over}"}}"#)).is_err());
    }

    #[test]
    fn rfc012c_effective_state_fixture_parses_and_preserves_identities() {
        let parsed =
            parse_rfc012c_effective_state_v1_json(RFC012C_EFFECTIVE_STATE_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value =
            serde_json::from_str(RFC012C_EFFECTIVE_STATE_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: EffectiveStateFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(fixture.dimension, EffectiveStateDimension::Model);
        assert_eq!(
            fixture.configured.evidence_kind,
            EffectiveStateEvidenceKind::ConfiguredIntent
        );
        assert_eq!(
            fixture.configured.value.authority,
            crate::adapter::EffectiveStateValueAuthority::NativeConfiguration
        );
        assert_eq!(
            fixture.configured.value.value.as_deref(),
            Some("claude-sonnet")
        );
        assert_eq!(
            fixture.observed.evidence_kind,
            EffectiveStateEvidenceKind::ResponseObserved
        );
        assert_eq!(fixture.retract.operation, EffectiveStateOperation::Retract);
        assert_ne!(
            fixture.configured.semantic_revision_ref,
            fixture.observed.semantic_revision_ref
        );
        assert_ne!(
            fixture.observed.semantic_revision_ref,
            fixture.retract.semantic_revision_ref
        );
        for name in ["model", "effort", "session_mode", "permission_mode"] {
            let dimension: EffectiveStateDimension =
                serde_json::from_str(&format!("\"{name}\"")).unwrap();
            assert_eq!(effective_state_native_bytes(dimension), name.as_bytes());
        }
    }

    #[test]
    fn rfc012c_effective_state_dimensions_are_independent_and_evidence_typed() {
        let cases = [
            (
                RFC012C_EFFECTIVE_STATE_FIXTURE,
                EffectiveStateDimension::Model,
                EffectiveStateEvidenceKind::ResponseObserved,
            ),
            (
                RFC012C_EFFECTIVE_STATE_EFFORT_FIXTURE,
                EffectiveStateDimension::Effort,
                EffectiveStateEvidenceKind::ResponseObserved,
            ),
            (
                RFC012C_EFFECTIVE_STATE_SESSION_MODE_FIXTURE,
                EffectiveStateDimension::SessionMode,
                EffectiveStateEvidenceKind::NativeTransition,
            ),
            (
                RFC012C_EFFECTIVE_STATE_PERMISSION_MODE_FIXTURE,
                EffectiveStateDimension::PermissionMode,
                EffectiveStateEvidenceKind::NativeTransition,
            ),
        ];
        let mut fact_ids = BTreeSet::new();
        for (json, dimension, observed_evidence) in cases {
            let fixture = decode_rfc012c_effective_state_v1(json).unwrap();
            assert_eq!(fixture.dimension, dimension);
            assert_eq!(fixture.observed.evidence_kind, observed_evidence);
            assert_eq!(
                fixture.configured.value.authority,
                crate::adapter::EffectiveStateValueAuthority::NativeConfiguration
            );
            assert_eq!(
                fixture.observed.value.authority,
                if observed_evidence == EffectiveStateEvidenceKind::ResponseObserved {
                    crate::adapter::EffectiveStateValueAuthority::NativeResponse
                } else {
                    crate::adapter::EffectiveStateValueAuthority::NativeTransition
                }
            );
            assert!(fact_ids.insert(fixture.fact_id));
        }
        assert_eq!(fact_ids.len(), 4);
    }

    #[test]
    fn rfc012c_interaction_fixture_parses_rfc012c_section_11_lifecycle() {
        let parsed = parse_rfc012c_interaction_v1_json(RFC012C_INTERACTION_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value =
            serde_json::from_str(RFC012C_INTERACTION_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: InteractionFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(fixture.kind, UserInputKind::Choice);
        assert_eq!(fixture.pending.state, UserInputLifecycleState::Pending);
        assert_eq!(fixture.resolved.state, UserInputLifecycleState::Resolved);
        assert_eq!(fixture.failed.state, UserInputLifecycleState::Failed);
        assert_eq!(fixture.cancelled.state, UserInputLifecycleState::Cancelled);
        assert_eq!(fixture.retract.operation, UserInputOperation::Retract);
        assert_eq!(fixture.partial.completeness, ContractCompleteness::Partial);
        assert!(fixture.pending.result_reference.is_none());
        assert_eq!(
            fixture.resolved.result_reference.as_deref(),
            Some("continue")
        );
        assert_eq!(fixture.questions[0].prompt, "Which option should we take?");
        assert_ne!(
            fixture.pending.semantic_revision_ref,
            fixture.resolved.semantic_revision_ref
        );
        assert_ne!(
            fixture.failed.semantic_revision_ref,
            fixture.cancelled.semantic_revision_ref
        );
    }

    #[test]
    fn rfc012c_state_and_interaction_reject_identity_and_lifecycle_drift() {
        let mut fact_id =
            serde_json::from_str::<serde_json::Value>(RFC012C_EFFECTIVE_STATE_FIXTURE).unwrap();
        fact_id["fact_id"] = fact_id["session"].clone();
        assert!(parse_rfc012c_effective_state_v1_json(&fact_id.to_string()).is_err());

        let mut reused =
            serde_json::from_str::<serde_json::Value>(RFC012C_EFFECTIVE_STATE_FIXTURE).unwrap();
        reused["observed"]["semantic_revision_ref"] =
            reused["configured"]["semantic_revision_ref"].clone();
        reused["observed"]["semantic_revision_key_hex"] =
            reused["configured"]["semantic_revision_key_hex"].clone();
        assert!(parse_rfc012c_effective_state_v1_json(&reused.to_string()).is_err());

        let mut extra =
            serde_json::from_str::<serde_json::Value>(RFC012C_EFFECTIVE_STATE_FIXTURE).unwrap();
        extra["future"] = serde_json::json!(true);
        assert!(parse_rfc012c_effective_state_v1_json(&extra.to_string()).is_err());

        let mut pending_result =
            serde_json::from_str::<serde_json::Value>(RFC012C_INTERACTION_FIXTURE).unwrap();
        pending_result["pending"]["result_reference"] = serde_json::json!("continue");
        assert!(parse_rfc012c_interaction_v1_json(&pending_result.to_string()).is_err());

        let mut resolved_missing =
            serde_json::from_str::<serde_json::Value>(RFC012C_INTERACTION_FIXTURE).unwrap();
        resolved_missing["resolved"]["result_reference"] = serde_json::Value::Null;
        assert!(parse_rfc012c_interaction_v1_json(&resolved_missing.to_string()).is_err());

        let mut native_payload =
            serde_json::from_str::<serde_json::Value>(RFC012C_INTERACTION_FIXTURE).unwrap();
        native_payload["native_payload"] = serde_json::json!({"prompt": "raw"});
        assert!(parse_rfc012c_interaction_v1_json(&native_payload.to_string()).is_err());

        let mut swapped =
            serde_json::from_str::<serde_json::Value>(RFC012C_INTERACTION_FIXTURE).unwrap();
        swapped["failed"]["semantic_revision_ref"] =
            swapped["cancelled"]["semantic_revision_ref"].clone();
        swapped["failed"]["semantic_revision_key_hex"] =
            swapped["cancelled"]["semantic_revision_key_hex"].clone();
        assert!(parse_rfc012c_interaction_v1_json(&swapped.to_string()).is_err());

        assert!(parse_rfc012c_effective_state_v1_json("").is_err());
        assert!(parse_rfc012c_interaction_v1_json("").is_err());
        let oversized = "x".repeat(MAX_SEMANTIC_FIXTURE_JSON_BYTES + 1);
        assert!(parse_rfc012c_effective_state_v1_json(&oversized).is_err());
        assert!(parse_rfc012c_interaction_v1_json(&oversized).is_err());
    }

    #[test]
    fn rfc012c_message_fixture_parses_correction_and_block_replacement() {
        let parsed = parse_rfc012c_message_v1_json(RFC012C_MESSAGE_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value = serde_json::from_str(RFC012C_MESSAGE_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: MessageFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(fixture.role, MessageRevisionRole::Assistant);
        assert_eq!(
            fixture.current.ordered_content_block_keys,
            ["block-a", "block-b"]
        );
        assert_eq!(
            fixture.correction.ordered_content_block_keys,
            ["block-a", "block-c"]
        );
        assert_eq!(
            fixture.complete_blocks.ordered_content_block_keys,
            ["block-a"]
        );
        assert_eq!(
            fixture.complete_blocks.completeness,
            ContractCompleteness::Complete
        );
        assert_eq!(
            fixture.partial_blocks.completeness,
            ContractCompleteness::Partial
        );
        assert_eq!(fixture.retract.operation, UserInputOperation::Retract);
        let content_block = fixture.content_block.as_ref().unwrap();
        assert_eq!(content_block.family, CONTENT_BLOCK_FAMILY);
        assert_eq!(content_block.current.ordinal, 0);
        assert_eq!(
            content_block.current.content,
            ContentBlockRevisionValue::Text {
                text: " draft \n".to_owned()
            }
        );
        assert_eq!(
            content_block.correction.content,
            ContentBlockRevisionValue::Text {
                text: " final answer \n".to_owned()
            }
        );
        assert_eq!(
            content_block.partial_retract.completeness,
            ContractCompleteness::Partial
        );
        assert_eq!(
            content_block.partial_retract.operation,
            UserInputOperation::Retract
        );
        assert_ne!(fixture.fact_id, content_block.fact_id);
        assert_ne!(
            fixture.current.semantic_revision_ref,
            fixture.correction.semantic_revision_ref
        );
        assert_ne!(
            fixture.complete_blocks.semantic_revision_ref,
            fixture.partial_blocks.semantic_revision_ref
        );

        let mut prior_v1: serde_json::Value =
            serde_json::from_str(RFC012C_MESSAGE_FIXTURE).unwrap();
        prior_v1.as_object_mut().unwrap().remove("content_block");
        let prior_round_trip = parse_rfc012c_message_v1_json(&prior_v1.to_string()).unwrap();
        let prior_round_trip: serde_json::Value = serde_json::from_str(&prior_round_trip).unwrap();
        assert_eq!(prior_round_trip, prior_v1);

        let mut explicit_null: serde_json::Value =
            serde_json::from_str(RFC012C_MESSAGE_FIXTURE).unwrap();
        explicit_null["content_block"] = serde_json::Value::Null;
        assert!(parse_rfc012c_message_v1_json(&explicit_null.to_string()).is_err());
    }

    #[test]
    fn rfc012c_task_fixture_parses_lifecycle_and_owned_set_omission() {
        let parsed = parse_rfc012c_task_v1_json(RFC012C_TASK_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value = serde_json::from_str(RFC012C_TASK_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: TaskFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(fixture.created.state, TaskLifecycleState::Created);
        assert_eq!(fixture.updated.state, TaskLifecycleState::Updated);
        assert_eq!(fixture.completed.state, TaskLifecycleState::Completed);
        assert_eq!(fixture.retract.operation, UserInputOperation::Retract);
        assert_eq!(fixture.partial.completeness, ContractCompleteness::Partial);
        assert_eq!(
            fixture.collection_omit.owned_set,
            Some(vec!["fixture-task-2".to_owned()])
        );
        assert_ne!(fixture.fact_id, fixture.peer_fact_id);
        assert_ne!(
            fixture.created.semantic_revision_ref,
            fixture.completed.semantic_revision_ref
        );
    }

    #[test]
    fn rfc012c_plan_fixture_parses_step_replacement_and_owned_set_omission() {
        let parsed = parse_rfc012c_plan_v1_json(RFC012C_PLAN_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value = serde_json::from_str(RFC012C_PLAN_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: PlanFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(fixture.current.ordered_step_keys, ["step-a", "step-b"]);
        assert_eq!(fixture.complete_steps.ordered_step_keys, ["step-a"]);
        assert_eq!(
            fixture.complete_steps.completeness,
            ContractCompleteness::Complete
        );
        assert_eq!(
            fixture.partial_steps.completeness,
            ContractCompleteness::Partial
        );
        assert_eq!(fixture.retract.operation, UserInputOperation::Retract);
        assert_eq!(
            fixture.collection_omit.owned_set,
            Some(vec!["fixture-plan-2".to_owned()])
        );
        assert_ne!(fixture.fact_id, fixture.peer_fact_id);
        assert_ne!(
            fixture.current.semantic_revision_ref,
            fixture.complete_steps.semantic_revision_ref
        );
    }

    #[test]
    fn rfc012c_tool_fixture_parses_correlation_and_unmatched_result() {
        let parsed = parse_rfc012c_tool_v1_json(RFC012C_TOOL_FIXTURE).unwrap();
        assert_privacy_safe(&parsed);
        let original: serde_json::Value = serde_json::from_str(RFC012C_TOOL_FIXTURE).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(round_trip, original);
        let fixture: ToolFixtureWire = serde_json::from_str(&parsed).unwrap();
        assert_eq!(fixture.call.kind, ToolRevisionKind::Call);
        assert_eq!(fixture.unmatched_result.kind, ToolRevisionKind::Result);
        assert_eq!(fixture.unmatched_result.correlated_native_id, None);
        assert_eq!(
            fixture.correlated_call.correlated_native_id.as_deref(),
            Some("fixture-result-1")
        );
        assert_eq!(
            fixture.correlated_result.correlated_native_id.as_deref(),
            Some("fixture-call-1")
        );
        assert_eq!(fixture.retract.operation, UserInputOperation::Retract);
        assert_eq!(fixture.partial.completeness, ContractCompleteness::Partial);
        assert_ne!(fixture.fact_id, fixture.result_fact_id);
        assert_ne!(
            fixture.call.semantic_revision_ref,
            fixture.correlated_call.semantic_revision_ref
        );
    }

    #[test]
    fn rfc012c_c1_fixtures_reject_semantic_mutation_with_stale_identity() {
        let mut effective: serde_json::Value =
            serde_json::from_str(RFC012C_EFFECTIVE_STATE_FIXTURE).unwrap();
        effective["configured"]["value"]["value"] = serde_json::json!("claude-opus");
        assert!(parse_rfc012c_effective_state_v1_json(&effective.to_string()).is_err());

        let mut interaction: serde_json::Value =
            serde_json::from_str(RFC012C_INTERACTION_FIXTURE).unwrap();
        interaction["questions"][0]["prompt"] =
            serde_json::json!("Which different option should we take?");
        assert!(parse_rfc012c_interaction_v1_json(&interaction.to_string()).is_err());

        let mut message: serde_json::Value = serde_json::from_str(RFC012C_MESSAGE_FIXTURE).unwrap();
        message["role"] = serde_json::json!("user");
        assert!(parse_rfc012c_message_v1_json(&message.to_string()).is_err());

        let mut content_block: serde_json::Value =
            serde_json::from_str(RFC012C_MESSAGE_FIXTURE).unwrap();
        content_block["content_block"]["correction"]["content"]["text"] =
            serde_json::json!("stale identity");
        assert!(parse_rfc012c_message_v1_json(&content_block.to_string()).is_err());

        let mut task: serde_json::Value = serde_json::from_str(RFC012C_TASK_FIXTURE).unwrap();
        task["subject"] = serde_json::json!("Different task subject");
        assert!(parse_rfc012c_task_v1_json(&task.to_string()).is_err());

        let mut plan: serde_json::Value = serde_json::from_str(RFC012C_PLAN_FIXTURE).unwrap();
        plan["subject"] = serde_json::json!("Different plan subject");
        assert!(parse_rfc012c_plan_v1_json(&plan.to_string()).is_err());

        let mut tool: serde_json::Value = serde_json::from_str(RFC012C_TOOL_FIXTURE).unwrap();
        tool["tool_name"] = serde_json::json!("write");
        assert!(parse_rfc012c_tool_v1_json(&tool.to_string()).is_err());
    }
}
