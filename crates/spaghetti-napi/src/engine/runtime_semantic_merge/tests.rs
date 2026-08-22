use serde::Deserialize;
use serde_json::{json, Value};

use super::*;
use crate::adapter::CoverageComparison;
use crate::semantic_contract::{
    parse_rfc012c_runtime_v1_json, RuntimeContractFixtureWire, UsageExampleWire,
};

const RFC012A: &str = include_str!("../../../fixtures/contracts/rfc012a-v1.json");
const RFC012C: &str = include_str!("../../../fixtures/contracts/rfc012c-runtime-v1.json");
const RFC012C_INTERACTION: &str =
    include_str!("../../../fixtures/contracts/rfc012c-interaction-v1.json");
const RFC012D_USAGE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-usage-envelope-v1.json");

#[derive(Deserialize)]
struct EnvelopeEventExtract {
    fact_id: CanonicalFactId,
    operation: String,
    retraction: Option<Value>,
    revision: UsageRevisionV2Fact,
}

#[derive(Deserialize)]
struct EnvelopeExtract {
    event_id: String,
    semantic_revision_ref: SemanticRevisionRef,
    event: EnvelopeEventExtract,
}

#[derive(Deserialize)]
struct UsageEnvelopeFixtureFile {
    upsert: EnvelopeExtract,
    reset_retraction: EnvelopeExtract,
}

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

fn observer_event_from_envelope(envelope: &EnvelopeExtract) -> ScopedUsageObserverEvent {
    let operation = match envelope.event.operation.as_str() {
        "upsert" => ScopedUsageOperation::Upsert,
        "retract" => ScopedUsageOperation::Retract,
        other => panic!("unsupported envelope operation {other}"),
    };
    let retraction = envelope
        .event
        .retraction
        .as_ref()
        .map(|value| match value["kind"].as_str() {
            Some("reset") => ScopedUsageRetraction::Reset {
                old_generation: value["old_generation"].as_u64().expect("old_generation"),
                new_generation: value["new_generation"].as_u64().expect("new_generation"),
            },
            Some("source_deleted") => ScopedUsageRetraction::SourceDeleted {
                generation: value["generation"].as_u64().expect("generation"),
            },
            other => panic!("unsupported retraction {other:?}"),
        });
    ScopedUsageObserverEvent {
        event_id: envelope.event_id.clone(),
        fact_id: envelope.event.fact_id,
        semantic_revision_ref: envelope.semantic_revision_ref,
        operation,
        retraction,
        revision: envelope.event.revision.clone(),
    }
}

fn usage_envelopes() -> UsageEnvelopeFixtureFile {
    serde_json::from_str(RFC012D_USAGE).expect("rfc012d usage envelope fixture")
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
    let envelopes = usage_envelopes();
    let upsert = observer_event_from_envelope(&envelopes.upsert);
    let reset = observer_event_from_envelope(&envelopes.reset_retraction);
    assert_eq!(upsert.fact_id, reset.fact_id);
    assert!(matches!(
        reset.retraction,
        Some(ScopedUsageRetraction::Reset { .. })
    ));
    assert_eq!(upsert.event_id, envelopes.upsert.event_id);
    assert_eq!(
        upsert.semantic_revision_ref,
        envelopes.upsert.semantic_revision_ref
    );

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
}

#[test]
fn merge_consumes_typed_usage_and_interaction_values_without_native_payloads() {
    let envelopes: Value = serde_json::from_str(RFC012D_USAGE).expect("envelope json");
    assert_eq!(
        envelopes["upsert"]["native_evidence"]["kind"],
        json!("withheld")
    );
    assert_eq!(
        envelopes["expected"]["native_payload_disclosure"],
        json!("withheld_at_projection_boundary")
    );
    assert!(envelopes["upsert"].get("native_payload").is_none());

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
    engine.shutdown().unwrap();
}
