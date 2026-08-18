use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{
    ActorAffiliationDimension, ActorAffiliationRevisionFact, ActorAffiliationState,
    ActorRunRevisionFact, ActorRunRole, AdapterId, ContractVersionOffer, ContractVersionRequest,
    Fact, FactBatch, FactSemanticContext, NativeIdentity, QualifiedValue, TimestampQuality,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
};
use crate::source::{RecordHash, RecordOrigin, SourceMediaType, SourceRecord};

use super::super::*;
use super::*;

const FROZEN_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-actor-envelope-v1.json");

fn contract_selection() -> ObservationContractSelection {
    let families = BTreeMap::from([
        (ACTOR_AFFILIATION_FAMILY.to_owned(), vec![1]),
        (ACTOR_RUN_FAMILY.to_owned(), vec![1]),
    ]);
    let request = ObservationContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: families.clone(),
            query_pack_versions: None,
            observation_contract_versions: Some(vec![1]),
        },
        vec![1],
        vec![1],
        vec![1],
    )
    .unwrap();
    let offer = ObservationContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: families,
            query_pack_versions: Vec::new(),
            observation_contract_versions: vec![1],
        },
        vec![1],
        vec![1],
        vec![1],
    )
    .unwrap();
    negotiate_observation_contract(&request, &offer).unwrap()
}

fn semantic_context() -> FactSemanticContext {
    FactSemanticContext::new(
        &AdapterId::new("fixture").unwrap(),
        1,
        b"stable-source-instance",
        b"transcript",
        b"root-session.jsonl",
        1,
    )
    .unwrap()
}

fn source_identity() -> ScopedSourceObjectIdentity {
    ScopedSourceObjectIdentity::from_semantic_context(&semantic_context()).unwrap()
}

fn root_identity() -> ScopedObservationRootIdentity {
    let context = semantic_context();
    let session_key = CanonicalEntityKey::derive(
        context.adapter_id().as_str(),
        &context.source_instance_key(),
        "session",
        b"native-session",
    )
    .unwrap();
    let session_ref = ExternalEntityRef::new(session_key);
    let identity = QualifiedValue::from_parts(
        Some(NativeIdentity {
            native_namespace: "fixture.session".to_owned(),
            native_id: "native-session".to_owned(),
        }),
        QualifiedValueQuality::NativeClaimed,
        "fixture".to_owned(),
        ContractCompleteness::Complete,
        None,
        None,
        Vec::new(),
    )
    .unwrap();
    ScopedRootIdentityRequest::new(
        1,
        b"stable-source-instance".as_slice(),
        b"native-session".as_slice(),
        None,
        Some(session_key),
        Some(session_ref),
    )
    .with_native_session_claim(NativeIdentityClaim::new(session_ref, identity).unwrap())
    .resolve(context.adapter_id(), 1)
    .unwrap()
}

fn source_record() -> SourceRecord {
    SourceRecord::new(
        &RecordOrigin {
            source_instance_id: 11,
            stream_id: 22,
            object_id: 33,
            observed_at: 44,
            source_timestamp_hint: Some(43),
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        },
        1,
        SourceCursor::append_offset(10),
        SourceCursor::append_offset(20),
        0,
        br#"{"type":"actor"}"#.to_vec(),
    )
}

fn actor_revision(batch: &FactBatch) -> ActorRunRevisionFact {
    let root = root_identity();
    ActorRunRevisionFact {
        actor_run: batch.canonical_entity_key("run", b"child-agent").unwrap(),
        session: root.session_key,
        role: ActorRunRole::Child,
        parent_actor_run: Some(root.root_actor_run_key),
        native_session_id: Some("native-session".to_owned()),
        native_actor_id: Some("child-agent".to_owned()),
        native_actor_type: Some("subagent".to_owned()),
    }
}

fn affiliation_revision(batch: &FactBatch) -> ActorAffiliationRevisionFact {
    let actor = actor_revision(batch);
    ActorAffiliationRevisionFact {
        affiliation: batch
            .canonical_entity_key("actor_affiliation", b"child-agent/workflow/workflow-main")
            .unwrap(),
        actor_run: actor.actor_run,
        session: actor.session,
        dimension: ActorAffiliationDimension::Workflow,
        target: batch
            .canonical_entity_key("workflow", b"workflow-main")
            .unwrap(),
        member: None,
        native_target_id: Some("workflow-main".to_owned()),
        native_member_id: None,
        state: ActorAffiliationState::Present,
        effective_at: Some(QualifiedTimestamp {
            value: "2026-08-18T00:00:00Z".to_owned(),
            quality: TimestampQuality::NativeExact,
        }),
    }
}

fn source_for_fact(
    record: &SourceRecord,
    source: ScopedSourceObjectIdentity,
    semantic: FactSemanticRevision,
    provenance: FactProvenance,
) -> ScopedUsageV2Source {
    ScopedUsageV2Source {
        object: source,
        source_record_id: semantic.source_record_id,
        provenance,
        cursor_start: record.cursor_start.clone(),
        cursor_end: record.cursor_end.clone(),
        ordinal_in_batch: record.ordinal_in_batch,
        source_timestamp_hint: record.source_timestamp_hint,
        media_type: record.media_type.clone(),
        state: record.state,
        payload_hash: RecordHash::digest(&record.payload),
    }
}

fn actor_ref(revision: &ActorRunRevisionFact) -> ScopedActorRunRef {
    ScopedActorRunRef {
        root_session_key: revision.session,
        run_key: revision.actor_run,
        role: revision.role,
        parent_run_key: revision.parent_actor_run,
        native_session_id: revision.native_session_id.clone(),
        native_actor_id: revision.native_actor_id.clone(),
        native_actor_type: revision.native_actor_type.clone(),
    }
}

fn retraction(
    operation: ScopedRevisionedEntityOperation,
) -> Option<ScopedRevisionedEntityRetractionCause> {
    match operation {
        ScopedRevisionedEntityOperation::Upsert => None,
        ScopedRevisionedEntityOperation::Retract => Some(
            ScopedRevisionedEntityRetractionCause::Reset(ScopedAppendReset {
                old_generation: 1,
                new_generation: 2,
                reason: AppendTransition::Truncated,
            }),
        ),
    }
}

fn phase(operation: ScopedRevisionedEntityOperation) -> ScopedAppendDeliveryPhase {
    match operation {
        ScopedRevisionedEntityOperation::Upsert => ScopedAppendDeliveryPhase::Bootstrap,
        ScopedRevisionedEntityOperation::Retract => ScopedAppendDeliveryPhase::Correction,
    }
}

fn actor_mapped_envelope(
    operation: ScopedRevisionedEntityOperation,
) -> (
    ScopedObservationEnvelope,
    ObservationContractSelection,
    ScopedObservationRootIdentity,
    ScopedSourceObjectIdentity,
) {
    let record = source_record();
    let mut batch = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
    let revision = actor_revision(&batch);
    batch
        .push_native(
            &record,
            b"child-agent",
            Fact::ActorRunRevision(revision.clone()),
        )
        .unwrap();
    let fact = &batch.facts()[0];
    let semantic = fact.semantic_revision.unwrap();
    let retraction = retraction(operation);
    let phase = phase(operation);
    let event_id = revisioned_entity_event_id(
        ACTOR_RUN_FAMILY.as_bytes(),
        operation,
        &semantic,
        retraction,
    );
    let source = source_identity();
    let event = ScopedActorRunEvent {
        event_id,
        semantic_revision_ref: semantic.semantic_revision_ref,
        fact_id: semantic.fact_id,
        operation,
        phase,
        observed_at: if operation == ScopedRevisionedEntityOperation::Upsert {
            record.observed_at
        } else {
            55
        },
        source: source_for_fact(&record, source.clone(), semantic, fact.provenance.clone()),
        retraction,
        actor: actor_ref(&revision),
        affiliations: unknown_actor_affiliation_context(revision.actor_run),
        revision,
    };
    let selection = contract_selection();
    let root = root_identity();
    let delivered = ScopedDeliveredObservation {
        event_contract_version: selection.event_contract_version,
        observer_sequence: if operation == ScopedRevisionedEntityOperation::Upsert {
            1
        } else {
            3
        },
        scope_epoch: 1,
        event_id,
        semantic_revision_ref: Some(semantic.semantic_revision_ref),
        phase,
        source: source.clone(),
        event: ScopedProjectedObservation::ActorRun {
            lane_ordinal: 1,
            event: Box::new(event),
        },
    };
    let envelope = ScopedObservationEnvelopeMapper::new(root.clone(), selection.clone())
        .map(delivered)
        .unwrap();
    (envelope, selection, root, source)
}

fn affiliation_mapped_envelope(
    operation: ScopedRevisionedEntityOperation,
) -> (
    ScopedObservationEnvelope,
    ObservationContractSelection,
    ScopedObservationRootIdentity,
    ScopedSourceObjectIdentity,
) {
    let record = source_record();
    let mut batch = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
    let revision = affiliation_revision(&batch);
    batch
        .push_native(
            &record,
            b"child-agent/workflow/workflow-main",
            Fact::ActorAffiliationRevision(revision.clone()),
        )
        .unwrap();
    let fact = &batch.facts()[0];
    let semantic = fact.semantic_revision.unwrap();
    let retraction = retraction(operation);
    let phase = phase(operation);
    let event_id = revisioned_entity_event_id(
        ACTOR_AFFILIATION_FAMILY.as_bytes(),
        operation,
        &semantic,
        retraction,
    );
    let source = source_identity();
    let actor = actor_revision(&batch);
    let event = ScopedActorAffiliationEvent {
        event_id,
        semantic_revision_ref: semantic.semantic_revision_ref,
        fact_id: semantic.fact_id,
        operation,
        phase,
        observed_at: if operation == ScopedRevisionedEntityOperation::Upsert {
            record.observed_at
        } else {
            55
        },
        source: source_for_fact(&record, source.clone(), semantic, fact.provenance.clone()),
        retraction,
        actor: Some(actor_ref(&actor)),
        context: ScopedActorAffiliationContext {
            actor_run_key: revision.actor_run,
            team_key: None,
            native_team_id: None,
            team_name: None,
            member_key: None,
            workflow_key: Some(revision.target),
            native_workflow_id: revision.native_target_id.clone(),
            completeness: ContractCompleteness::Partial,
            derived_from_revision_refs: vec![semantic.semantic_revision_ref],
        },
        revision,
    };
    let selection = contract_selection();
    let root = root_identity();
    let delivered = ScopedDeliveredObservation {
        event_contract_version: selection.event_contract_version,
        observer_sequence: if operation == ScopedRevisionedEntityOperation::Upsert {
            2
        } else {
            4
        },
        scope_epoch: 1,
        event_id,
        semantic_revision_ref: Some(semantic.semantic_revision_ref),
        phase,
        source: source.clone(),
        event: ScopedProjectedObservation::ActorAffiliation {
            lane_ordinal: 1,
            event: Box::new(event),
        },
    };
    let envelope = ScopedObservationEnvelopeMapper::new(root.clone(), selection.clone())
        .map(delivered)
        .unwrap();
    (envelope, selection, root, source)
}

fn context_value(
    selection: &ObservationContractSelection,
    root: &ScopedObservationRootIdentity,
    source: &ScopedSourceObjectIdentity,
) -> Value {
    json!({
        "contract_selection": selection,
        "root": {
            "session_ref": root.session_ref,
            "session_key": root.session_key,
            "root_actor_run_key": root.root_actor_run_key,
            "native_session_claim": root.native_session_claim.clone(),
        },
        "authorized_sources": [{
            "instance_key": source.source_instance_key,
            "stream_key": source.stream_key,
            "object_key": source.object_key,
        }],
    })
}

fn fixture_value() -> Value {
    let (actor, selection, root, source) =
        actor_mapped_envelope(ScopedRevisionedEntityOperation::Upsert);
    let (retraction, _, _, _) =
        affiliation_mapped_envelope(ScopedRevisionedEntityOperation::Retract);
    json!({
        "fixture_contract_version": 1,
        "context": context_value(&selection, &root, &source),
        "actor_upsert": ScopedActorEnvelopeWire::from_scoped(&actor).unwrap(),
        "affiliation_reset_retraction": ScopedActorEnvelopeWire::from_scoped(&retraction).unwrap(),
        "expected": {
            "fact_families": [ACTOR_AFFILIATION_FAMILY, ACTOR_RUN_FAMILY],
            "fact_family_contract_version": 1,
            "complete_event_union": false,
            "unsupported_variants": "usage_source_and_observer_lifecycle_controls",
            "native_payload_disclosure": "withheld_at_projection_boundary",
            "portable_transport": false,
        },
    })
}

fn parse_for_context(
    value: Value,
    selection: &ObservationContractSelection,
    root: &ScopedObservationRootIdentity,
    source: &ScopedSourceObjectIdentity,
) -> Result<ScopedActorEnvelopeWire, ScopedActorEnvelopeContractError> {
    ScopedActorEnvelopeWire::from_wire_value_for_context(
        value,
        selection,
        root,
        std::slice::from_ref(source),
    )
}

#[test]
fn frozen_rust_actor_fixture_is_stable_and_contextual() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    if FROZEN_FIXTURE == "{}\n" {
        println!("{actual}");
    }
    assert_eq!(actual, FROZEN_FIXTURE);
    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    let (_, selection, root, source) =
        actor_mapped_envelope(ScopedRevisionedEntityOperation::Upsert);
    for field in ["actor_upsert", "affiliation_reset_retraction"] {
        let parsed = parse_for_context(fixture[field].clone(), &selection, &root, &source).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), fixture[field]);
    }
}

#[test]
fn both_families_and_operations_round_trip_but_other_events_do_not() {
    for operation in [
        ScopedRevisionedEntityOperation::Upsert,
        ScopedRevisionedEntityOperation::Retract,
    ] {
        for (envelope, selection, root, source) in [
            actor_mapped_envelope(operation),
            affiliation_mapped_envelope(operation),
        ] {
            let wire = ScopedActorEnvelopeWire::from_scoped(&envelope).unwrap();
            let encoded = serde_json::to_value(&wire).unwrap();
            assert_eq!(
                parse_for_context(encoded.clone(), &selection, &root, &source).unwrap(),
                wire
            );
            assert_eq!(encoded["native_evidence"]["kind"], json!("withheld"));
            assert_eq!(
                encoded["native_evidence"]["reason"],
                json!("projection_boundary")
            );
            assert!(encoded["native_evidence"].get("payload").is_none());
            assert!(encoded["source"]["locator_id"].is_null());
        }
    }
    let (mut usage, _, _, _) = actor_mapped_envelope(ScopedRevisionedEntityOperation::Upsert);
    usage.event = ScopedObservationEvent::SourcePresence {
        change: ScopedAppendPresenceChange::Created { generation: 1 },
    };
    assert_eq!(
        ScopedActorEnvelopeWire::from_scoped(&usage).unwrap_err(),
        ScopedActorEnvelopeContractError::UnsupportedEvent
    );
}

#[test]
fn semantic_value_family_and_event_identity_are_recomputed() {
    let (envelope, selection, root, source) =
        affiliation_mapped_envelope(ScopedRevisionedEntityOperation::Upsert);
    let value =
        serde_json::to_value(ScopedActorEnvelopeWire::from_scoped(&envelope).unwrap()).unwrap();
    for mutator in [
        |value: &mut Value| value["event"]["revision"]["state"] = json!("removed"),
        |value: &mut Value| value["event"]["fact_family"] = json!(ACTOR_RUN_FAMILY),
        |value: &mut Value| value["event_id"] = json!(encode_opaque(&[9; 32])),
    ] {
        let mut changed = value.clone();
        mutator(&mut changed);
        assert!(parse_for_context(changed, &selection, &root, &source).is_err());
    }
    let mut missing_self = value;
    missing_self["affiliations"]["derived_from_revision_refs"] = json!([]);
    assert!(parse_for_context(missing_self, &selection, &root, &source).is_err());
}

#[test]
fn exact_selection_root_source_and_operation_evidence_fail_closed() {
    let (envelope, selection, root, source) =
        actor_mapped_envelope(ScopedRevisionedEntityOperation::Upsert);
    let value =
        serde_json::to_value(ScopedActorEnvelopeWire::from_scoped(&envelope).unwrap()).unwrap();
    let mut family = value.clone();
    family["contract_selection"]["contract_versions"]["fact_family_versions"][ACTOR_RUN_FAMILY] =
        json!(2);
    assert!(parse_for_context(family, &selection, &root, &source).is_err());
    let mut root_drift = value.clone();
    root_drift["root"]["root_actor_run_key"] = json!(root.session_key);
    assert!(parse_for_context(root_drift, &selection, &root, &source).is_err());
    let mut source_drift = value.clone();
    source_drift["source"]["object_key"] = json!(root.session_key);
    assert!(parse_for_context(source_drift, &selection, &root, &source).is_err());
    let mut false_retraction = value.clone();
    false_retraction["event"]["operation"] = json!("retract");
    assert!(parse_for_context(false_retraction, &selection, &root, &source).is_err());
    let mut false_evidence = value;
    false_evidence["evidence"]["authority"] = json!("common_reducer");
    assert!(parse_for_context(false_evidence, &selection, &root, &source).is_err());
}

#[test]
fn strict_nested_shapes_bounds_and_privacy_fail_closed() {
    let (envelope, selection, root, source) =
        affiliation_mapped_envelope(ScopedRevisionedEntityOperation::Upsert);
    let value =
        serde_json::to_value(ScopedActorEnvelopeWire::from_scoped(&envelope).unwrap()).unwrap();
    for path in [
        &["root", "native_session_claim"][..],
        &["root", "native_session_claim", "identity"][..],
        &["root", "native_session_claim", "identity", "value"][..],
        &["native_time"][..],
        &["event", "revision"][..],
        &["event", "revision", "effective_at"][..],
    ] {
        let mut changed = value.clone();
        let mut target = &mut changed;
        for segment in path {
            target = &mut target[*segment];
        }
        target["future_meaning"] = json!(true);
        assert!(parse_for_context(changed, &selection, &root, &source).is_err());
    }
    let mut locator = value.clone();
    locator["source"]["locator_id"] = json!("/Users/private/session.jsonl");
    assert!(parse_for_context(locator, &selection, &root, &source).is_err());
    let mut whitespace = value.clone();
    whitespace["event"]["revision"]["native_target_id"] = json!(" workflow-main");
    assert!(parse_for_context(whitespace, &selection, &root, &source).is_err());
    let mut unsafe_sequence = value.clone();
    unsafe_sequence["observer_sequence"] = json!(JS_SAFE_INTEGER_MAX + 1);
    assert!(parse_for_context(unsafe_sequence, &selection, &root, &source).is_err());
    let mut oversized_refs = value;
    oversized_refs["affiliations"]["derived_from_revision_refs"] = json!(vec![
        json!({
            "semantic_reference_contract_version": 1,
            "fact_revision_id": encode_opaque(&[1; 32]),
        });
        MAX_AFFILIATION_REVISIONS
            + 1
    ]);
    assert!(parse_for_context(oversized_refs, &selection, &root, &source).is_err());
}
