use serde_json::{json, Value};

use super::*;
use crate::adapter::{
    ContentBlockRevisionFact, CoverageComparison, EffectiveStateDimension,
    EffectiveStateEvidenceKind, EffectiveStateRevisionFact, MessageRevisionFact,
    MessageRevisionRole, NativeRuntimeMarkerRevisionFact, PlanRevisionFact, TaskLifecycleState,
    TaskRevisionFact, ToolRevisionFact, UserInputRequestRevisionFact,
};
use crate::semantic_contract::{
    decode_rfc012c_effective_state_v1, decode_rfc012c_message_v1, decode_rfc012c_native_marker_v1,
    decode_rfc012c_plan_v1, decode_rfc012c_task_v1, decode_rfc012c_tool_v1,
    parse_rfc012c_runtime_v1_json, RuntimeContractFixtureWire, UsageExampleWire,
};

const RFC012A: &str = include_str!("../../../fixtures/contracts/rfc012a-v1.json");
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

fn coverage_sets() -> (SourceCoverageSet, SourceCoverageSet, SourceCoverageSet) {
    let value: Value = serde_json::from_str(RFC012A).expect("rfc012a fixture");
    (
        serde_json::from_value(value["coverage"]["baseline"].clone()).expect("baseline coverage"),
        serde_json::from_value(value["coverage"]["dominant"].clone()).expect("dominant coverage"),
        serde_json::from_value(value["coverage"]["reset"].clone()).expect("reset coverage"),
    )
}

fn with_completeness(set: &SourceCoverageSet, completeness: &str) -> SourceCoverageSet {
    let mut value = serde_json::to_value(set).expect("serialize coverage");
    value["completeness"] = json!(completeness);
    serde_json::from_value(value).expect("coverage with completeness")
}

fn contribution(example: &UsageExampleWire) -> DurableUsageContribution {
    DurableUsageContribution {
        fact_id: example.fact_id,
        semantic_revision_ref: example.semantic_revision_ref,
        revision: example.revision.clone(),
    }
}

fn upsert_event(event_id: &str, example: &UsageExampleWire) -> ScopedUsageObserverEvent {
    ScopedUsageObserverEvent {
        event_id: event_id.to_owned(),
        fact_id: example.fact_id,
        semantic_revision_ref: example.semantic_revision_ref,
        operation: ScopedUsageOperation::Upsert,
        retraction: None,
        revision: example.revision.clone(),
    }
}

/// A retraction event for the same fact, as the observer emits one when the
/// object that owned it resets or disappears.
fn retract_event(
    event_id: &str,
    example: &UsageExampleWire,
    retraction: ScopedUsageRetraction,
) -> ScopedUsageObserverEvent {
    ScopedUsageObserverEvent {
        event_id: event_id.to_owned(),
        fact_id: example.fact_id,
        semantic_revision_ref: example.semantic_revision_ref,
        operation: ScopedUsageOperation::Retract,
        retraction: Some(retraction),
        revision: example.revision.clone(),
    }
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

fn rebind_semantic_revision(
    semantic: FactSemanticRevision,
    revision_key: [u8; 32],
) -> FactSemanticRevision {
    let fact_revision_id = crate::adapter::FactRevisionId::derive(
        &semantic.fact_id,
        RuntimeSemanticFamily::VERSION,
        &revision_key,
    )
    .expect("semantic revision identity");
    FactSemanticRevision {
        fact_revision_id,
        semantic_revision_ref: SemanticRevisionRef::new(fact_revision_id),
        ..semantic
    }
}

fn coverage_for_family(family: RuntimeSemanticFamily, completeness: &str) -> SourceCoverageSet {
    let (baseline, _, _) = coverage_sets();
    let mut value = serde_json::to_value(baseline).expect("coverage json");
    let domain = json!({
        "kind": "fact_family",
        "family": family.as_str(),
        "version": RuntimeSemanticFamily::VERSION,
    });
    value["coverage_domain"] = domain.clone();
    for point in value["points"].as_array_mut().expect("coverage points") {
        point["coverage_domain"] = domain.clone();
    }
    value["completeness"] = json!(completeness);
    serde_json::from_value(value).expect("family coverage")
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

#[test]
fn all_runtime_families_reconcile_with_equal_replacement_state_digests() {
    let contributions = all_runtime_family_contributions();
    assert_eq!(contributions.len(), 11, "closed runtime family inventory");

    for (family, contribution) in contributions {
        let expected_digest = runtime_replacement_state_digest(
            family,
            [RuntimeReplacementDigestEntity {
                semantic: &contribution.semantic,
                revision: &contribution.revision,
            }],
        )
        .expect("typed replacement digest");
        let partial = coverage_for_family(family, "partial");
        let event = ScopedRuntimeObserverEvent {
            event_id: format!("evt-{}", family.as_str()),
            semantic: contribution.semantic,
            operation: ScopedRuntimeOperation::Upsert,
            retraction: None,
            revision: contribution.revision.clone(),
        };

        let overlay = merge_durable_and_scoped_runtime(
            &[],
            &partial,
            &[event.clone(), event.clone()],
            &partial,
        )
        .expect("partial overlay merge");
        assert_eq!(overlay.family, family);
        assert_eq!(
            overlay.overlay,
            OverlayDisposition::Retained { stale: true }
        );
        assert_eq!(overlay.contributions.len(), 1);
        assert_eq!(
            overlay.contributions[0].origin,
            MergedContributionOrigin::Overlay
        );
        assert_eq!(overlay.replacement_state_digest, expected_digest);
        assert_eq!(overlay.delivered_observer_occurrences.len(), 1);

        let complete = coverage_for_family(family, "complete");
        let retired = merge_durable_and_scoped_runtime(
            std::slice::from_ref(&contribution),
            &complete,
            &[event],
            &complete,
        )
        .expect("complete durable merge");
        assert_eq!(retired.overlay, OverlayDisposition::Retired);
        assert_eq!(retired.contributions.len(), 1);
        assert_eq!(
            retired.contributions[0].origin,
            MergedContributionOrigin::Durable
        );
        assert_eq!(retired.replacement_state_digest, expected_digest);
    }
}

#[test]
fn all_family_merge_rejects_family_and_operation_drift() {
    let mut contributions = all_runtime_family_contributions();
    let (_, actor) = contributions.remove(0);
    let usage_coverage = coverage_for_family(RuntimeSemanticFamily::UsageV2, "partial");
    assert!(
        merge_durable_and_scoped_runtime(&[actor], &usage_coverage, &[], &usage_coverage,).is_err()
    );

    let (_, task) = contributions
        .into_iter()
        .find(|(family, _)| *family == RuntimeSemanticFamily::Task)
        .expect("task contribution");
    let task_coverage = coverage_for_family(RuntimeSemanticFamily::Task, "partial");
    let mismatched = ScopedRuntimeObserverEvent {
        event_id: "evt-task-mismatched-operation".to_string(),
        semantic: task.semantic,
        operation: ScopedRuntimeOperation::Retract,
        retraction: None,
        revision: task.revision,
    };
    assert!(
        merge_durable_and_scoped_runtime(&[], &task_coverage, &[mismatched], &task_coverage,)
            .is_err()
    );
}

#[test]
fn all_family_merge_applies_stable_identity_and_partial_retraction_laws() {
    let contributions = all_runtime_family_contributions();
    let (_, actor) = contributions
        .iter()
        .find(|(family, _)| *family == RuntimeSemanticFamily::ActorRun)
        .expect("actor contribution");
    let Fact::ActorRunRevision(mut drifted_actor) = actor.revision.clone() else {
        panic!("actor fact")
    };
    drifted_actor.session = drifted_actor.actor_run;
    let drifted_actor_semantic = rebind_semantic_revision(
        actor.semantic,
        drifted_actor.semantic_revision_key().expect("actor key"),
    );
    let actor_coverage = coverage_for_family(RuntimeSemanticFamily::ActorRun, "partial");
    let drifted_actor_event = ScopedRuntimeObserverEvent {
        event_id: "evt-actor-retarget".to_string(),
        semantic: drifted_actor_semantic,
        operation: ScopedRuntimeOperation::Upsert,
        retraction: None,
        revision: Fact::ActorRunRevision(drifted_actor),
    };
    assert!(merge_durable_and_scoped_runtime(
        std::slice::from_ref(actor),
        &actor_coverage,
        &[drifted_actor_event],
        &actor_coverage,
    )
    .is_err());

    let (_, task) = contributions
        .iter()
        .find(|(family, _)| *family == RuntimeSemanticFamily::Task)
        .expect("task contribution");
    let Fact::TaskRevision(mut partial_retract) = task.revision.clone() else {
        panic!("task fact")
    };
    partial_retract.operation = crate::adapter::UserInputOperation::Retract;
    partial_retract.completeness = ContractCompleteness::Partial;
    partial_retract.owned_set = None;
    let partial_retract_semantic = rebind_semantic_revision(
        task.semantic,
        partial_retract.semantic_revision_key().expect("task key"),
    );
    let task_coverage = coverage_for_family(RuntimeSemanticFamily::Task, "partial");
    let partial_retract_event = ScopedRuntimeObserverEvent {
        event_id: "evt-task-partial-retract".to_string(),
        semantic: partial_retract_semantic,
        operation: ScopedRuntimeOperation::Retract,
        retraction: None,
        revision: Fact::TaskRevision(partial_retract),
    };
    let retained = merge_durable_and_scoped_runtime(
        std::slice::from_ref(task),
        &task_coverage,
        &[partial_retract_event],
        &task_coverage,
    )
    .expect("partial retraction is non-authoritative");
    assert_eq!(retained.contributions.len(), 1);
    assert_eq!(
        retained.contributions[0].origin,
        MergedContributionOrigin::Durable
    );
    assert_eq!(retained.contributions[0].semantic, task.semantic);

    let Fact::TaskRevision(mut complete_retract) = task.revision.clone() else {
        panic!("task fact")
    };
    complete_retract.operation = crate::adapter::UserInputOperation::Retract;
    complete_retract.completeness = ContractCompleteness::Complete;
    complete_retract.owned_set = None;
    let complete_retract_semantic = rebind_semantic_revision(
        task.semantic,
        complete_retract.semantic_revision_key().expect("task key"),
    );
    let complete_retract_event = ScopedRuntimeObserverEvent {
        event_id: "evt-task-complete-retract".to_string(),
        semantic: complete_retract_semantic,
        operation: ScopedRuntimeOperation::Retract,
        retraction: None,
        revision: Fact::TaskRevision(complete_retract),
    };
    let retracted = merge_durable_and_scoped_runtime(
        std::slice::from_ref(task),
        &task_coverage,
        &[complete_retract_event],
        &task_coverage,
    )
    .expect("complete retraction is authoritative");
    assert!(retracted.contributions.is_empty());
    assert_eq!(
        retracted.replacement_state_digest,
        runtime_replacement_state_digest(
            RuntimeSemanticFamily::Task,
            std::iter::empty::<RuntimeReplacementDigestEntity<'_>>(),
        )
        .expect("empty task digest")
    );
}

#[test]
fn ordered_observer_delivery_deduplicates_by_event_id_and_preserves_a_b_a() {
    let fixture = runtime_fixture();
    let aba = &fixture.usage.response_revisions;
    assert_eq!(
        aba.a.semantic_revision_ref,
        aba.a_repeat.semantic_revision_ref
    );
    assert_ne!(aba.a.semantic_revision_ref, aba.b.semantic_revision_ref);
    assert_eq!(aba.a.fact_id, aba.b.fact_id);
    assert_eq!(aba.a.fact_id, aba.a_repeat.fact_id);

    let (baseline, _, _) = coverage_sets();
    let partial = with_completeness(&baseline, "partial");
    let merged = merge_durable_and_scoped_usage(
        &[],
        &baseline,
        &[
            upsert_event("evt-a-1", &aba.a),
            upsert_event("evt-b", &aba.b),
            upsert_event("evt-a-1", &aba.a),
            upsert_event("evt-a-2", &aba.a_repeat),
        ],
        &partial,
    )
    .expect("merge");

    assert_eq!(merged.overlay, OverlayDisposition::Retained { stale: true });
    assert_eq!(merged.delivered_observer_occurrences.len(), 3);
    assert_eq!(merged.delivered_observer_occurrences[0].event_id, "evt-a-1");
    assert_eq!(merged.delivered_observer_occurrences[1].event_id, "evt-b");
    assert_eq!(merged.delivered_observer_occurrences[2].event_id, "evt-a-2");
    assert_eq!(
        merged.delivered_observer_occurrences[0].semantic_revision_ref,
        merged.delivered_observer_occurrences[2].semantic_revision_ref
    );
    assert_eq!(merged.contributions.len(), 1);
    assert_eq!(merged.contributions[0].fact_id, aba.a.fact_id);
    assert_eq!(
        merged.contributions[0].semantic_revision_ref,
        aba.a_repeat.semantic_revision_ref
    );
    assert_eq!(merged.contributions[0].revision, aba.a_repeat.revision);
    assert_eq!(
        merged.contributions[0].origin,
        MergedContributionOrigin::Overlay
    );
}

#[test]
fn merge_rejects_conflicting_event_ids_and_unbound_usage_revisions() {
    let fixture = runtime_fixture();
    let aba = &fixture.usage.response_revisions;
    let (baseline, _, _) = coverage_sets();
    let partial = with_completeness(&baseline, "partial");

    let first = upsert_event("evt-conflict", &aba.a);
    let conflicting = upsert_event("evt-conflict", &aba.b);
    assert!(merge_durable_and_scoped_usage(
        &[],
        &baseline,
        &[first.clone(), conflicting],
        &partial,
    )
    .is_err());

    let mut drifted_event = first;
    drifted_event.semantic_revision_ref = aba.b.semantic_revision_ref;
    let drifted_event_result =
        merge_durable_and_scoped_usage(&[], &baseline, &[drifted_event], &partial);
    assert!(drifted_event_result.is_err());

    let drifted_durable = DurableUsageContribution {
        fact_id: aba.a.fact_id,
        semantic_revision_ref: aba.b.semantic_revision_ref,
        revision: aba.a.revision.clone(),
    };
    let drifted_durable_result =
        merge_durable_and_scoped_usage(&[drifted_durable], &baseline, &[], &partial);
    assert!(drifted_durable_result.is_err());
}

#[test]
fn merge_rejects_duplicate_durable_facts_and_non_usage_coverage() {
    let fixture = runtime_fixture();
    let (baseline, _, _) = coverage_sets();
    let partial = with_completeness(&baseline, "partial");
    let contribution = contribution(&fixture.usage.response_revisions.a);
    assert!(merge_durable_and_scoped_usage(
        &[contribution.clone(), contribution],
        &baseline,
        &[],
        &partial,
    )
    .is_err());

    let mut decode_value = serde_json::to_value(&baseline).expect("coverage json");
    decode_value["coverage_domain"] = json!({"kind": "decode"});
    for point in decode_value["points"]
        .as_array_mut()
        .expect("coverage points")
    {
        point["coverage_domain"] = json!({"kind": "decode"});
    }
    let decode_coverage: SourceCoverageSet =
        serde_json::from_value(decode_value).expect("valid decode coverage");
    let wrong_domain_result =
        merge_durable_and_scoped_usage(&[], &decode_coverage, &[], &decode_coverage);
    assert!(wrong_domain_result.is_err());
}

#[test]
fn overlay_replaces_one_fact_identity_while_preserving_semantic_revision_refs() {
    let fixture = runtime_fixture();
    let aba = &fixture.usage.response_revisions;
    let (baseline, _, _) = coverage_sets();
    let partial = with_completeness(&baseline, "partial");
    let durable = vec![contribution(&aba.a)];
    let merged = merge_durable_and_scoped_usage(
        &durable,
        &baseline,
        &[upsert_event("evt-b", &aba.b)],
        &partial,
    )
    .expect("merge");
    assert_eq!(merged.contributions.len(), 1);
    assert_eq!(merged.contributions[0].fact_id, aba.a.fact_id);
    assert_eq!(merged.contributions[0].fact_id, aba.b.fact_id);
    assert_eq!(
        merged.contributions[0].semantic_revision_ref,
        aba.b.semantic_revision_ref
    );
    assert_ne!(
        merged.contributions[0].semantic_revision_ref,
        aba.a.semantic_revision_ref
    );
    assert_eq!(
        merged.delivered_observer_occurrences[0].semantic_revision_ref,
        aba.b.semantic_revision_ref
    );
}

#[test]
fn complete_equal_or_durable_dominating_coverage_retires_overlay() {
    let fixture = runtime_fixture();
    let durable = vec![contribution(&fixture.usage.native_message)];
    let overlay_event = upsert_event("evt-fallback", &fixture.usage.source_record_fallback);
    let (baseline, dominant, _) = coverage_sets();
    assert_eq!(
        compare_coverage(&dominant, &baseline).unwrap(),
        CoverageComparison::Dominates
    );
    assert_eq!(
        compare_coverage(&baseline, &baseline).unwrap(),
        CoverageComparison::Equal
    );

    let equal = merge_durable_and_scoped_usage(
        &durable,
        &baseline,
        std::slice::from_ref(&overlay_event),
        &baseline,
    )
    .expect("equal merge");
    assert_eq!(equal.overlay, OverlayDisposition::Retired);
    assert_eq!(equal.contributions.len(), 1);
    assert_eq!(
        equal.contributions[0].fact_id,
        fixture.usage.native_message.fact_id
    );
    assert_eq!(
        equal.contributions[0].origin,
        MergedContributionOrigin::Durable
    );
    assert!(!equal
        .contributions
        .iter()
        .any(|item| item.fact_id == fixture.usage.source_record_fallback.fact_id));

    assert_eq!(
        compare_coverage(&baseline, &dominant).unwrap(),
        CoverageComparison::Behind
    );
    let durable_dominating =
        merge_durable_and_scoped_usage(&durable, &dominant, &[overlay_event], &baseline)
            .expect("durable-dominating merge");
    assert_eq!(durable_dominating.overlay, OverlayDisposition::Retired);
    assert_eq!(durable_dominating.contributions.len(), 1);
    assert_eq!(
        durable_dominating.contributions[0].origin,
        MergedContributionOrigin::Durable
    );
}

#[test]
fn complete_observer_dominating_coverage_retains_current_overlay() {
    let fixture = runtime_fixture();
    let durable = vec![contribution(&fixture.usage.native_message)];
    let overlay_event = upsert_event("evt-current", &fixture.usage.source_record_fallback);
    let (baseline, dominant, _) = coverage_sets();

    assert_eq!(
        compare_coverage(&dominant, &baseline).unwrap(),
        CoverageComparison::Dominates
    );
    let merged = merge_durable_and_scoped_usage(&durable, &baseline, &[overlay_event], &dominant)
        .expect("observer-dominating merge");

    assert_eq!(
        merged.overlay,
        OverlayDisposition::Retained { stale: false }
    );
    assert_eq!(merged.contributions.len(), 2);
    assert!(merged.contributions.iter().any(|item| {
        item.fact_id == fixture.usage.source_record_fallback.fact_id
            && item.origin == MergedContributionOrigin::Overlay
    }));
}

#[test]
fn incomplete_durable_coverage_cannot_retire_an_equal_overlay() {
    let fixture = runtime_fixture();
    let durable = vec![contribution(&fixture.usage.native_message)];
    let overlay_event = upsert_event(
        "evt-equal-incomplete",
        &fixture.usage.source_record_fallback,
    );
    let (baseline, _, _) = coverage_sets();
    let incomplete_durable = with_completeness(&baseline, "partial");

    let merged =
        merge_durable_and_scoped_usage(&durable, &incomplete_durable, &[overlay_event], &baseline)
            .expect("incomplete-durable merge");

    assert_eq!(
        merged.overlay,
        OverlayDisposition::Retained { stale: false }
    );
    assert_eq!(merged.contributions.len(), 2);
}

#[test]
fn partial_unavailable_or_incomparable_coverage_retains_stale_overlay() {
    let fixture = runtime_fixture();
    let durable = vec![contribution(&fixture.usage.native_message)];
    let overlay_event = upsert_event("evt-fallback", &fixture.usage.source_record_fallback);
    let (baseline, _, reset) = coverage_sets();
    assert_eq!(
        compare_coverage(&reset, &baseline).unwrap(),
        CoverageComparison::Incomparable
    );

    for observer_coverage in [
        with_completeness(&baseline, "partial"),
        with_completeness(&baseline, "unavailable"),
        reset,
    ] {
        let merged = merge_durable_and_scoped_usage(
            &durable,
            &baseline,
            std::slice::from_ref(&overlay_event),
            &observer_coverage,
        )
        .expect("stale merge");
        assert_eq!(merged.overlay, OverlayDisposition::Retained { stale: true });
        assert_eq!(merged.contributions.len(), 2);
        assert_eq!(
            merged.contributions[0].fact_id,
            fixture.usage.native_message.fact_id
        );
        assert_eq!(
            merged.contributions[0].origin,
            MergedContributionOrigin::Durable
        );
        assert_eq!(
            merged.contributions[1].fact_id,
            fixture.usage.source_record_fallback.fact_id
        );
        assert_eq!(
            merged.contributions[1].origin,
            MergedContributionOrigin::Overlay
        );
    }
}

#[test]
fn reset_retraction_applies_before_replay() {
    let fixture = runtime_fixture();
    let example = &fixture.usage.native_message;
    let upsert = upsert_event("evt-upsert", example);
    let reset = retract_event(
        "evt-reset",
        example,
        ScopedUsageRetraction::Reset {
            old_generation: 1,
            new_generation: 2,
        },
    );
    assert_eq!(upsert.fact_id, reset.fact_id);
    assert_eq!(upsert.semantic_revision_ref, reset.semantic_revision_ref);

    let durable = vec![DurableUsageContribution {
        fact_id: upsert.fact_id,
        semantic_revision_ref: upsert.semantic_revision_ref,
        revision: upsert.revision.clone(),
    }];
    let (baseline, _, _) = coverage_sets();
    let partial = with_completeness(&baseline, "partial");
    let reset_event_id = reset.event_id.clone();
    let upsert_event_id = upsert.event_id.clone();
    let merged =
        merge_durable_and_scoped_usage(&durable, &baseline, &[upsert.clone(), reset], &partial)
            .expect("reset-before-replay merge");

    assert_eq!(merged.overlay, OverlayDisposition::Retained { stale: true });
    assert_eq!(merged.delivered_observer_occurrences.len(), 2);
    assert_eq!(
        merged.delivered_observer_occurrences[0].event_id,
        reset_event_id
    );
    assert_eq!(
        merged.delivered_observer_occurrences[1].event_id,
        upsert_event_id
    );
    assert_eq!(merged.contributions.len(), 1);
    assert_eq!(merged.contributions[0].fact_id, upsert.fact_id);
    assert_eq!(merged.contributions[0].revision, upsert.revision);
    assert_eq!(
        merged.contributions[0].origin,
        MergedContributionOrigin::Overlay
    );

    // A deleted source object retracts the same way a reset does.
    let deleted = retract_event(
        "evt-deleted",
        example,
        ScopedUsageRetraction::SourceDeleted { generation: 2 },
    );
    let after_delete =
        merge_durable_and_scoped_usage(&durable, &baseline, &[upsert, deleted], &partial)
            .expect("source-deleted merge");
    assert_eq!(after_delete.delivered_observer_occurrences.len(), 2);
    assert_eq!(
        after_delete.delivered_observer_occurrences[0].event_id,
        "evt-deleted"
    );
}

#[test]
fn merge_consumes_typed_usage_and_interaction_values_without_native_payloads() {
    let interaction = parse_rfc012c_interaction_v1_json(RFC012C_INTERACTION).expect("interaction");
    assert_eq!(interaction.family, "runtime.user-input-request");
    assert_eq!(interaction.pending.state, UserInputLifecycleState::Pending);
    assert_eq!(
        interaction.resolved.state,
        UserInputLifecycleState::Resolved
    );
    assert_eq!(interaction.failed.state, UserInputLifecycleState::Failed);
    assert_eq!(
        interaction.cancelled.state,
        UserInputLifecycleState::Cancelled
    );
    assert_eq!(interaction.retract.operation, UserInputOperation::Retract);
    assert_eq!(
        interaction.partial.completeness,
        crate::adapter::ContractCompleteness::Partial
    );
    assert_eq!(interaction.pending.fact_id, interaction.resolved.fact_id);
    assert_ne!(
        interaction.pending.semantic_revision_ref,
        interaction.resolved.semantic_revision_ref
    );
    assert_eq!(
        interaction.pending.questions[0].prompt,
        "Which option should we take?"
    );
    assert_eq!(
        interaction.pending.questions[0].options[0].label,
        "Continue"
    );
    assert_eq!(
        interaction.pending.questions[0].options[1]
            .preview
            .as_deref(),
        Some("Abort the in-flight plan")
    );
    assert!(interaction.pending.result_reference.is_none());
    assert_eq!(
        interaction.resolved.result_reference.as_deref(),
        Some("continue")
    );

    let mut native_payload = serde_json::from_str::<Value>(RFC012C_INTERACTION).unwrap();
    native_payload["native_payload"] = json!({"prompt": "raw"});
    assert!(parse_rfc012c_interaction_v1_json(&native_payload.to_string()).is_err());
}

#[test]
fn engine_query_service_is_the_durable_live_merge_consumer() {
    use crate::engine::{EngineOptions, SpaghettiEngineCore};

    let fixture = runtime_fixture();
    let (baseline, _, _) = coverage_sets();
    let partial = with_completeness(&baseline, "partial");
    let events = [upsert_event(
        "evt-live-1",
        &fixture.usage.response_revisions.a,
    )];

    let dir = tempfile::tempdir().unwrap();
    let mut options = EngineOptions {
        database_path: dir.path().join("merge-consumer.db"),
        query_workers: Some(1),
        owner_label: Some("merge-consumer".to_string()),
        defer_query_structures: true,
        source_pass_pool: None,
    };
    let bootstrapping = SpaghettiEngineCore::open(options.clone()).unwrap();
    let bootstrapping_merged = bootstrapping
        .merge_runtime_usage_live(&[], &baseline, &events, &partial)
        .expect("typed merge is not gated on FTS bootstrap");
    assert_eq!(
        bootstrapping_merged.overlay,
        OverlayDisposition::Retained { stale: true }
    );
    bootstrapping.shutdown().unwrap();

    options.defer_query_structures = false;
    let engine = SpaghettiEngineCore::open(options).unwrap();
    let merged = engine
        .merge_runtime_usage_live(&[], &baseline, &events, &partial)
        .expect("live query pool must reach durable/scoped merge");
    assert_eq!(merged.overlay, OverlayDisposition::Retained { stale: true });
    assert_eq!(merged.delivered_observer_occurrences.len(), 1);
    assert_eq!(
        merged.delivered_observer_occurrences[0].event_id,
        "evt-live-1"
    );

    let (family, actor) = all_runtime_family_contributions()
        .into_iter()
        .find(|(family, _)| *family == RuntimeSemanticFamily::ActorRun)
        .expect("actor contribution");
    let actor_coverage = coverage_for_family(family, "partial");
    let actor_event = ScopedRuntimeObserverEvent {
        event_id: "evt-live-actor".to_string(),
        semantic: actor.semantic,
        operation: ScopedRuntimeOperation::Upsert,
        retraction: None,
        revision: actor.revision,
    };
    let all_family = engine
        .merge_runtime_semantic_live(&[], &actor_coverage, &[actor_event], &actor_coverage)
        .expect("engine reaches all-family typed merge");
    assert_eq!(all_family.family, RuntimeSemanticFamily::ActorRun);
    assert_eq!(all_family.contributions.len(), 1);
    engine.shutdown().unwrap();
}
