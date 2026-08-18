use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, ContractVersionOffer, ContractVersionRequest, Fact, FactBatch, FactSemanticContext,
    NativeIdentity, QualifiedValue, TimestampQuality, UsageBucketsV2, UsageResponseIdentity,
    UsageValueAuthority, UsageValueProvenance,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
};
use crate::source::{RecordHash, RecordOrigin, SourceMediaType, SourceRecord};

use super::super::*;
use super::*;

const FROZEN_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-usage-envelope-v1.json");

fn contract_selection() -> ObservationContractSelection {
    let families = BTreeMap::from([(USAGE_FAMILY.to_owned(), vec![1])]);
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

fn exact_value<T>(value: T, native_field: &str) -> UsageQualifiedValue<T> {
    QualifiedValue::from_parts(
        Some(value),
        QualifiedValueQuality::Exact,
        UsageValueAuthority::NativeResponse,
        ContractCompleteness::Complete,
        None,
        None,
        UsageValueProvenance {
            native_field: native_field.to_owned(),
            normalization_contract_version: 1,
        },
    )
    .unwrap()
}

fn usage_revision(batch: &FactBatch) -> UsageRevisionV2Fact {
    UsageRevisionV2Fact {
        session: batch
            .canonical_entity_key("session", b"native-session")
            .unwrap(),
        actor_run: batch
            .canonical_entity_key("actor-run", b"native-run")
            .unwrap(),
        response_key: b"message-1".to_vec(),
        response_identity: UsageResponseIdentity::NativeMessageId,
        native_message_id: Some("message-1".to_owned()),
        request_id: Some("request-1".to_owned()),
        buckets: UsageBucketsV2 {
            input_tokens: exact_value(11, "message.usage.input_tokens"),
            output_tokens: exact_value(12, "message.usage.output_tokens"),
            cache_creation_input_tokens: exact_value(
                13,
                "message.usage.cache_creation_input_tokens",
            ),
            cache_read_input_tokens: exact_value(14, "message.usage.cache_read_input_tokens"),
        },
        model: Some(exact_value("model-1".to_owned(), "message.model")),
        effort: None,
        source_time: Some(QualifiedTimestamp {
            value: "2026-08-18T00:00:00Z".to_owned(),
            quality: TimestampQuality::NativeExact,
        }),
    }
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
        br#"{"type":"result","usage":{}}"#.to_vec(),
    )
}

fn mapped_envelope(
    operation: ScopedUsageV2Operation,
) -> (
    ScopedObservationEnvelope,
    ObservationContractSelection,
    ScopedObservationRootIdentity,
    ScopedSourceObjectIdentity,
) {
    let record = source_record();
    let mut batch = FactBatch::new_with_semantic_context(4, 4, semantic_context()).unwrap();
    let revision = usage_revision(&batch);
    let revision_key = revision.semantic_revision_key().unwrap();
    batch
        .push_native_object_scoped_with_revision(
            &record,
            b"message-1",
            &revision_key,
            Fact::UsageRevisionV2(revision.clone()),
        )
        .unwrap();
    let fact = &batch.facts()[0];
    let semantic = fact.semantic_revision.unwrap();
    let retraction = match operation {
        ScopedUsageV2Operation::Upsert => None,
        ScopedUsageV2Operation::Retract => {
            Some(ScopedUsageV2RetractionCause::Reset(ScopedAppendReset {
                old_generation: 1,
                new_generation: 2,
                reason: AppendTransition::Truncated,
            }))
        }
    };
    let phase = match operation {
        ScopedUsageV2Operation::Upsert => ScopedAppendDeliveryPhase::Bootstrap,
        ScopedUsageV2Operation::Retract => ScopedAppendDeliveryPhase::Correction,
    };
    let observed_at = match operation {
        ScopedUsageV2Operation::Upsert => record.observed_at,
        ScopedUsageV2Operation::Retract => 55,
    };
    let event_id = usage_v2_event_id(operation, &semantic, retraction);
    let source = source_identity();
    let event = ScopedUsageV2Event {
        event_id,
        semantic_revision_ref: semantic.semantic_revision_ref,
        fact_id: semantic.fact_id,
        operation,
        phase,
        observed_at,
        source: ScopedUsageV2Source {
            object: source.clone(),
            source_record_id: semantic.source_record_id,
            provenance: fact.provenance.clone(),
            cursor_start: record.cursor_start.clone(),
            cursor_end: record.cursor_end.clone(),
            ordinal_in_batch: record.ordinal_in_batch,
            source_timestamp_hint: record.source_timestamp_hint,
            media_type: record.media_type.clone(),
            state: record.state,
            payload_hash: RecordHash::digest(&record.payload),
        },
        retraction,
        revision,
    };
    let selection = contract_selection();
    let root = root_identity();
    let projected = ScopedProjectedObservation::UsageV2 {
        lane_ordinal: 1,
        event: Box::new(event),
    };
    let delivered = ScopedDeliveredObservation {
        event_contract_version: selection.event_contract_version,
        observer_sequence: match operation {
            ScopedUsageV2Operation::Upsert => 1,
            ScopedUsageV2Operation::Retract => 2,
        },
        scope_epoch: 1,
        event_id,
        semantic_revision_ref: Some(semantic.semantic_revision_ref),
        phase,
        source: source.clone(),
        event: projected,
    };
    let envelope = ScopedObservationEnvelopeMapper::new(root.clone(), selection.clone())
        .map(delivered)
        .unwrap();
    (envelope, selection, root, source)
}

fn fixture_value() -> Value {
    let (upsert, selection, root, source) = mapped_envelope(ScopedUsageV2Operation::Upsert);
    let (retraction, _, _, _) = mapped_envelope(ScopedUsageV2Operation::Retract);
    let upsert = ScopedUsageEnvelopeWire::from_scoped(&upsert).unwrap();
    let retraction = ScopedUsageEnvelopeWire::from_scoped(&retraction).unwrap();
    json!({
        "fixture_contract_version": 1,
        "context": {
            "contract_selection": selection,
            "root": {
                "session_ref": root.session_ref,
                "session_key": root.session_key,
                "root_actor_run_key": root.root_actor_run_key,
                "native_session_claim": root.native_session_claim,
            },
            "authorized_sources": [{
                "instance_key": source.source_instance_key,
                "stream_key": source.stream_key,
                "object_key": source.object_key,
            }],
        },
        "upsert": upsert,
        "reset_retraction": retraction,
        "expected": {
            "fact_family": USAGE_FAMILY,
            "fact_family_contract_version": 1,
            "complete_event_union": false,
            "unsupported_variants": "source_and_observer_lifecycle_controls",
            "native_payload_disclosure": "withheld_at_projection_boundary",
        },
    })
}

fn parse_for_context(
    value: Value,
    selection: &ObservationContractSelection,
    root: &ScopedObservationRootIdentity,
    source: &ScopedSourceObjectIdentity,
) -> Result<ScopedUsageEnvelopeWire, ScopedUsageEnvelopeContractError> {
    ScopedUsageEnvelopeWire::from_wire_value_for_context(
        value,
        selection,
        root,
        std::slice::from_ref(source),
    )
}

#[test]
fn mapped_usage_upsert_and_retraction_round_trip_only_with_exact_context() {
    for operation in [
        ScopedUsageV2Operation::Upsert,
        ScopedUsageV2Operation::Retract,
    ] {
        let (envelope, selection, root, source) = mapped_envelope(operation);
        let wire = ScopedUsageEnvelopeWire::from_scoped(&envelope).unwrap();
        let parsed = parse_for_context(
            serde_json::to_value(&wire).unwrap(),
            &selection,
            &root,
            &source,
        )
        .unwrap();
        assert_eq!(parsed, wire);
    }
}

#[test]
fn received_envelopes_cannot_retarget_selection_root_or_source() {
    let (envelope, selection, root, source) = mapped_envelope(ScopedUsageV2Operation::Upsert);
    let wire = ScopedUsageEnvelopeWire::from_scoped(&envelope).unwrap();
    let value = serde_json::to_value(&wire).unwrap();

    let mut wrong_selection = value.clone();
    wrong_selection["contract_selection"]["event_contract_version"] = json!(2);
    assert!(parse_for_context(wrong_selection, &selection, &root, &source).is_err());

    let (_, _, other_root, _) = {
        let (envelope, selection, mut root, source) =
            mapped_envelope(ScopedUsageV2Operation::Upsert);
        root.root_actor_run_key = CanonicalEntityKey::derive_root_actor_run(
            root.adapter_id.as_str(),
            &root.source_instance_key,
            &root.session_key,
            Some(b"other-root"),
        )
        .unwrap();
        (envelope, selection, root, source)
    };
    assert!(parse_for_context(value.clone(), &selection, &other_root, &source).is_err());

    let mut foreign_source = source.clone();
    foreign_source.object_key =
        CoverageObjectKey::derive("fixture.other-object", b"other-object").unwrap();
    let oversized_sources = vec![source.clone(); MAX_AUTHORIZED_SOURCES + 1];
    assert!(ScopedUsageEnvelopeWire::from_wire_value_for_context(
        value.clone(),
        &selection,
        &root,
        &oversized_sources,
    )
    .is_err());
    assert!(parse_for_context(value, &selection, &root, &foreign_source).is_err());
}

#[test]
fn semantic_event_and_occurrence_fields_are_exact_and_portably_bounded() {
    let (envelope, selection, root, source) = mapped_envelope(ScopedUsageV2Operation::Upsert);
    let base =
        serde_json::to_value(ScopedUsageEnvelopeWire::from_scoped(&envelope).unwrap()).unwrap();
    let reject = |value| assert!(parse_for_context(value, &selection, &root, &source).is_err());

    let mut event_id = base.clone();
    event_id["event_id"] = json!(format!("v1:{}", "A".repeat(43)));
    reject(event_id);

    let mut revision = base.clone();
    revision["event"]["revision"]["buckets"]["input_tokens"]["value"] = json!(12);
    reject(revision);

    let mut source_record = base.clone();
    source_record["source"]["source_record_id"] = json!(format!("v1:{}", "A".repeat(43)));
    reject(source_record);

    let mut cursor = base.clone();
    cursor["source"]["byte_range"]["end"] = json!(21);
    reject(cursor);

    let mut unsafe_sequence = base.clone();
    unsafe_sequence["observer_sequence"] = json!(JS_SAFE_INTEGER_MAX + 1);
    reject(unsafe_sequence);

    let mut false_retraction = base.clone();
    false_retraction["event"]["operation"] = json!("retract");
    reject(false_retraction);

    let mut false_evidence = base.clone();
    false_evidence["evidence"]["authority"] = json!("common_reducer");
    reject(false_evidence);
}

#[test]
fn strict_nested_contracts_reject_unknown_fields_instead_of_silently_dropping_them() {
    let (envelope, selection, root, source) = mapped_envelope(ScopedUsageV2Operation::Upsert);
    let base =
        serde_json::to_value(ScopedUsageEnvelopeWire::from_scoped(&envelope).unwrap()).unwrap();

    for path in [
        &["root", "native_session_claim", "identity"][..],
        &["root", "native_session_claim", "identity", "value"][..],
        &["native_time"][..],
        &["event", "revision"][..],
        &["event", "revision", "buckets"][..],
        &["event", "revision", "buckets", "input_tokens", "provenance"][..],
        &["event", "retraction"][..],
    ] {
        let mut unknown = base.clone();
        let mut target = &mut unknown;
        for segment in path {
            target = &mut target[*segment];
        }
        target["future_meaning"] = json!(true);
        assert!(parse_for_context(unknown, &selection, &root, &source).is_err());
    }

    for path in [
        &["root", "native_session_claim"][..],
        &["actor", "parent_run_key"][..],
        &["affiliations", "team_key"][..],
        &["source", "locator_id"][..],
        &["native_time"][..],
        &["evidence", "effective_at"][..],
        &["event", "retraction"][..],
        &["event", "revision", "request_id"][..],
        &["event", "revision", "model"][..],
        &["event", "revision", "effort"][..],
        &["event", "revision", "source_time"][..],
    ] {
        let mut missing = base.clone();
        let (field, parents) = path.split_last().unwrap();
        let mut target = &mut missing;
        for segment in parents {
            target = &mut target[*segment];
        }
        target.as_object_mut().unwrap().remove(*field);
        assert!(parse_for_context(missing, &selection, &root, &source).is_err());
    }
}

#[test]
fn specialized_text_fields_are_bounded_and_canonical() {
    let (envelope, selection, root, source) = mapped_envelope(ScopedUsageV2Operation::Upsert);
    let base =
        serde_json::to_value(ScopedUsageEnvelopeWire::from_scoped(&envelope).unwrap()).unwrap();

    let mut native_identity = base.clone();
    native_identity["root"]["native_session_claim"]["identity"]["value"]["native_id"] =
        json!("x".repeat(MAX_IDENTIFIER_BYTES + 1));
    assert!(parse_for_context(native_identity, &selection, &root, &source).is_err());

    let mut revision =
        usage_revision(&FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap());
    revision.request_id = Some("x".repeat(MAX_RUNTIME_TEXT_BYTES + 1));
    assert!(validate_usage_revision(&revision).is_err());
    revision.request_id = Some("request-1".to_owned());
    revision.model = Some(exact_value(
        "x".repeat(MAX_RUNTIME_TEXT_BYTES + 1),
        "message.model",
    ));
    assert!(validate_usage_revision(&revision).is_err());
}

#[test]
fn specialized_wire_rejects_unfrozen_event_or_native_payload_variants() {
    let (mut envelope, _, _, _) = mapped_envelope(ScopedUsageV2Operation::Upsert);
    envelope.event = ScopedObservationEvent::SourcePresence {
        change: ScopedAppendPresenceChange::Created { generation: 1 },
    };
    assert_eq!(
        ScopedUsageEnvelopeWire::from_scoped(&envelope).unwrap_err(),
        ScopedUsageEnvelopeContractError::UnsupportedEvent
    );

    let (mut envelope, _, _, _) = mapped_envelope(ScopedUsageV2Operation::Upsert);
    envelope.native_evidence = ScopedNativeEvidence::InlineSourceRecord {
        media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        state: SourceRecordState::Present,
        payload_hash: RecordHash::digest(b"native"),
        payload: b"native".to_vec(),
    };
    assert_eq!(
        ScopedUsageEnvelopeWire::from_scoped(&envelope).unwrap_err(),
        ScopedUsageEnvelopeContractError::UnsupportedEvent
    );
}

#[test]
fn frozen_rust_usage_envelope_fixture_is_stable() {
    let actual = fixture_value();
    let expected: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    if actual != expected {
        eprintln!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
    assert_eq!(actual, expected);
}
