use super::*;
use crate::adapter::{
    CanonicalFactId, ContentBlockRevisionFact, EffectiveStateDimension, EffectiveStateEvidenceKind,
    EffectiveStateRevisionFact, MessageRevisionFact, MessageRevisionRole,
    NativeRuntimeMarkerRevisionFact, PlanRevisionFact, SemanticRevisionRef, TaskLifecycleState,
    TaskRevisionFact, ToolRevisionFact, UserInputRequestRevisionFact,
};
use crate::runtime_semantic_reducer::RuntimeSemanticFamily;
use crate::semantic_contract::{
    decode_rfc012c_effective_state_v1, decode_rfc012c_interaction_v1, decode_rfc012c_message_v1,
    decode_rfc012c_native_marker_v1, decode_rfc012c_plan_v1, decode_rfc012c_task_v1,
    decode_rfc012c_tool_v1, parse_rfc012c_runtime_v1_json, RuntimeContractFixtureWire,
};

const RFC012C: &str = include_str!("../../../fixtures/contracts/rfc012c-runtime-v1.json");
const RFC012C_INTERACTION: &str =
    include_str!("../../../fixtures/contracts/rfc012c-interaction-v1.json");
const RFC012C_EFFECTIVE_STATE: &str =
    include_str!("../../../fixtures/contracts/rfc012c-effective-state-v1.json");
const RFC012C_MESSAGE: &str = include_str!("../../../fixtures/contracts/rfc012c-message-v1.json");
const RFC012C_NATIVE_MARKER: &str =
    include_str!("../../../fixtures/contracts/rfc012c-native-marker-v1.json");
const RFC012C_TASK: &str = include_str!("../../../fixtures/contracts/rfc012c-task-v1.json");
const RFC012C_PLAN: &str = include_str!("../../../fixtures/contracts/rfc012c-plan-v1.json");
const RFC012C_TOOL: &str = include_str!("../../../fixtures/contracts/rfc012c-tool-v1.json");
fn runtime_fixture() -> RuntimeContractFixtureWire {
    let parsed = parse_rfc012c_runtime_v1_json(RFC012C).expect("rfc012c fixture");
    serde_json::from_str(&parsed).expect("rfc012c typed fixture")
}

fn semantic_revision(
    fact_id: CanonicalFactId,
    source_record_id: crate::adapter::SourceRecordId,
    semantic_revision_ref: SemanticRevisionRef,
) -> FactSemanticRevision {
    FactSemanticRevision {
        source_record_id,
        fact_id,
        fact_revision_id: semantic_revision_ref.fact_revision_id,
        semantic_revision_ref,
    }
}

pub(crate) fn all_runtime_family_contributions(
) -> Vec<(RuntimeSemanticFamily, DurableRuntimeContribution)> {
    let runtime = runtime_fixture();
    let actor = &runtime.actors.root;
    let affiliation = &runtime.affiliations.child_team_present;
    let usage = &runtime.usage.native_message;

    let interaction = decode_rfc012c_interaction_v1(RFC012C_INTERACTION).expect("interaction");
    let interaction_fact = UserInputRequestRevisionFact {
        session: interaction.session,
        actor_run: interaction.actor_run,
        native_tool_use_id: interaction.native_tool_use_id.clone(),
        kind: interaction.kind,
        questions: interaction.questions.clone(),
        state: interaction.pending.state,
        operation: interaction.pending.operation,
        completeness: interaction.pending.completeness,
        result_reference: interaction.pending.result_reference.clone(),
    };

    let message = decode_rfc012c_message_v1(RFC012C_MESSAGE).expect("message");
    let message_role = match message.role {
        crate::semantic_contract::MessageRevisionRole::User => MessageRevisionRole::User,
        crate::semantic_contract::MessageRevisionRole::Assistant => MessageRevisionRole::Assistant,
        crate::semantic_contract::MessageRevisionRole::System => MessageRevisionRole::System,
    };
    let message_fact = MessageRevisionFact {
        session: message.session,
        actor_run: message.actor_run,
        native_message_id: message.native_message_id.clone(),
        role: message_role,
        ordered_content_block_keys: message.current.ordered_content_block_keys.clone(),
        completeness: message.current.completeness,
        operation: message.current.operation,
    };
    let content_block = message
        .content_block
        .as_ref()
        .expect("message fixture content block");
    let content_block_fact = ContentBlockRevisionFact {
        session: message.session,
        actor_run: message.actor_run,
        message: message.fact_id,
        native_content_block_id: Some(content_block.native_content_block_id.clone()),
        ordinal: content_block.current.ordinal,
        content: content_block.current.content.clone(),
        native_tool_call_or_result_id: content_block.current.native_tool_call_or_result_id.clone(),
        completeness: content_block.current.completeness,
        operation: content_block.current.operation,
    };

    let marker = decode_rfc012c_native_marker_v1(RFC012C_NATIVE_MARKER).expect("marker");
    let marker_example = &marker.progress;
    let marker_fact = NativeRuntimeMarkerRevisionFact {
        session: marker.session,
        actor_run: marker.actor_run,
        native_marker_id: marker_example.native_marker_id.clone(),
        correlated_native_id: marker_example.current.correlated_native_id.clone(),
        value: marker_example.current.value.clone(),
        quality: marker_example.current.quality,
        effective_at: marker_example.current.effective_at,
        provenance: marker_example.current.provenance.clone(),
        completeness: marker_example.current.completeness,
        operation: marker_example.current.operation,
    };

    let task = decode_rfc012c_task_v1(RFC012C_TASK).expect("task");
    let task_state = match task.created.state {
        crate::semantic_contract::TaskLifecycleState::Created => TaskLifecycleState::Created,
        crate::semantic_contract::TaskLifecycleState::Updated => TaskLifecycleState::Updated,
        crate::semantic_contract::TaskLifecycleState::Completed => TaskLifecycleState::Completed,
        crate::semantic_contract::TaskLifecycleState::Failed => TaskLifecycleState::Failed,
        crate::semantic_contract::TaskLifecycleState::Cancelled => TaskLifecycleState::Cancelled,
        crate::semantic_contract::TaskLifecycleState::Removed => TaskLifecycleState::Removed,
    };
    let task_fact = TaskRevisionFact {
        session: task.session,
        actor_run: task.actor_run,
        native_task_id: task.native_task_id.clone(),
        subject: task.subject.clone(),
        state: task_state,
        completeness: task.created.completeness,
        operation: task.created.operation,
        owned_set: task.created.owned_set.clone(),
    };

    let plan = decode_rfc012c_plan_v1(RFC012C_PLAN).expect("plan");
    let plan_fact = PlanRevisionFact {
        session: plan.session,
        actor_run: plan.actor_run,
        native_plan_id: plan.native_plan_id.clone(),
        subject: plan.subject.clone(),
        ordered_step_keys: plan.current.ordered_step_keys.clone(),
        completeness: plan.current.completeness,
        operation: plan.current.operation,
        owned_set: plan.current.owned_set.clone(),
    };

    let tool = decode_rfc012c_tool_v1(RFC012C_TOOL).expect("tool");
    let tool_fact = ToolRevisionFact {
        session: tool.session,
        actor_run: tool.actor_run,
        native_tool_id: tool.native_call_id.clone(),
        kind: tool.call.kind,
        tool_name: tool.tool_name.clone(),
        correlated_native_id: tool.call.correlated_native_id.clone(),
        completeness: tool.call.completeness,
        operation: tool.call.operation,
    };

    let effective =
        decode_rfc012c_effective_state_v1(RFC012C_EFFECTIVE_STATE).expect("effective state");
    let effective_dimension = match effective.dimension {
        crate::semantic_contract::EffectiveStateDimension::Model => EffectiveStateDimension::Model,
        crate::semantic_contract::EffectiveStateDimension::Effort => {
            EffectiveStateDimension::Effort
        }
        crate::semantic_contract::EffectiveStateDimension::SessionMode => {
            EffectiveStateDimension::SessionMode
        }
        crate::semantic_contract::EffectiveStateDimension::PermissionMode => {
            EffectiveStateDimension::PermissionMode
        }
    };
    let effective_kind = match effective.configured.evidence_kind {
        crate::semantic_contract::EffectiveStateEvidenceKind::ConfiguredIntent => {
            EffectiveStateEvidenceKind::ConfiguredIntent
        }
        crate::semantic_contract::EffectiveStateEvidenceKind::ResponseObserved => {
            EffectiveStateEvidenceKind::ResponseObserved
        }
        crate::semantic_contract::EffectiveStateEvidenceKind::NativeTransition => {
            EffectiveStateEvidenceKind::NativeTransition
        }
    };
    let effective_operation = match effective.configured.operation {
        crate::semantic_contract::EffectiveStateOperation::Upsert => {
            crate::adapter::UserInputOperation::Upsert
        }
        crate::semantic_contract::EffectiveStateOperation::Retract => {
            crate::adapter::UserInputOperation::Retract
        }
    };
    let effective_fact = EffectiveStateRevisionFact {
        session: effective.session,
        actor_run: effective.actor_run,
        dimension: effective_dimension,
        value: effective.configured.value.clone(),
        evidence_kind: effective_kind,
        completeness: effective.configured.completeness,
        operation: effective_operation,
    };

    let contribution =
        |fact_id, source_record_id, semantic_revision_ref, revision| DurableRuntimeContribution {
            semantic: semantic_revision(fact_id, source_record_id, semantic_revision_ref),
            revision,
        };
    vec![
        (
            RuntimeSemanticFamily::ActorRun,
            contribution(
                actor.fact_id,
                actor.source_record_id,
                actor.semantic_revision_ref,
                Fact::ActorRunRevision(actor.revision.clone()),
            ),
        ),
        (
            RuntimeSemanticFamily::ActorAffiliation,
            contribution(
                affiliation.fact_id,
                affiliation.source_record_id,
                affiliation.semantic_revision_ref,
                Fact::ActorAffiliationRevision(affiliation.revision.clone()),
            ),
        ),
        (
            RuntimeSemanticFamily::UsageV2,
            contribution(
                usage.fact_id,
                usage.source_record_id,
                usage.semantic_revision_ref,
                Fact::UsageRevisionV2(usage.revision.clone()),
            ),
        ),
        (
            RuntimeSemanticFamily::UserInputRequest,
            contribution(
                interaction.fact_id,
                interaction.source_record_id,
                interaction.pending.semantic_revision_ref,
                Fact::UserInputRequestRevision(interaction_fact),
            ),
        ),
        (
            RuntimeSemanticFamily::Message,
            contribution(
                message.fact_id,
                message.source_record_id,
                message.current.semantic_revision_ref,
                Fact::MessageRevision(message_fact),
            ),
        ),
        (
            RuntimeSemanticFamily::ContentBlock,
            contribution(
                content_block.fact_id,
                message.source_record_id,
                content_block.current.semantic_revision_ref,
                Fact::ContentBlockRevision(content_block_fact),
            ),
        ),
        (
            RuntimeSemanticFamily::NativeMarker,
            contribution(
                marker_example.fact_id,
                marker.source_record_id,
                marker_example.current.semantic_revision_ref,
                Fact::NativeRuntimeMarkerRevision(marker_fact),
            ),
        ),
        (
            RuntimeSemanticFamily::Task,
            contribution(
                task.fact_id,
                task.source_record_id,
                task.created.semantic_revision_ref,
                Fact::TaskRevision(task_fact),
            ),
        ),
        (
            RuntimeSemanticFamily::Plan,
            contribution(
                plan.fact_id,
                plan.source_record_id,
                plan.current.semantic_revision_ref,
                Fact::PlanRevision(plan_fact),
            ),
        ),
        (
            RuntimeSemanticFamily::Tool,
            contribution(
                tool.fact_id,
                tool.source_record_id,
                tool.call.semantic_revision_ref,
                Fact::ToolRevision(tool_fact),
            ),
        ),
        (
            RuntimeSemanticFamily::EffectiveState,
            contribution(
                effective.fact_id,
                effective.source_record_id,
                effective.configured.semantic_revision_ref,
                Fact::EffectiveStateRevision(effective_fact),
            ),
        ),
    ]
}
