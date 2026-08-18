use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, ContractVersionOffer, ContractVersionRequest, FactSemanticContext, NativeIdentity,
    QualifiedValue,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
};

use super::super::*;
use super::*;

const FROZEN_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-source-envelope-v1.json");

#[derive(Debug, Clone, Copy)]
enum FixtureControl {
    Created,
    Deleted,
    Reset,
    RetryScheduled,
    RetryExhausted,
    TerminalError,
}

fn contract_selection() -> ObservationContractSelection {
    let families = BTreeMap::from([("runtime.usage-v2".to_owned(), vec![1])]);
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

fn mapped_envelope(
    control: FixtureControl,
) -> (
    ScopedObservationEnvelope,
    ObservationContractSelection,
    ScopedObservationRootIdentity,
    ScopedSourceObjectIdentity,
) {
    let selection = contract_selection();
    let root = root_identity();
    let source = source_identity();
    let (observer_sequence, phase, event_id, projected) = match control {
        FixtureControl::Created => {
            let change = ScopedAppendPresenceChange::Created { generation: 1 };
            let event_id = source_presence_event_id(&source, change);
            (
                1,
                ScopedAppendDeliveryPhase::Bootstrap,
                event_id,
                ScopedProjectedObservation::SourcePresence {
                    object_token: 7,
                    source: source.clone(),
                    lane_ordinal: 1,
                    observed_at: 41,
                    phase: ScopedAppendDeliveryPhase::Bootstrap,
                    event_id,
                    change,
                },
            )
        }
        FixtureControl::Deleted => {
            let change = ScopedAppendPresenceChange::Deleted { generation: 2 };
            let event_id = source_presence_event_id(&source, change);
            (
                2,
                ScopedAppendDeliveryPhase::Live,
                event_id,
                ScopedProjectedObservation::SourcePresence {
                    object_token: 7,
                    source: source.clone(),
                    lane_ordinal: 2,
                    observed_at: 42,
                    phase: ScopedAppendDeliveryPhase::Live,
                    event_id,
                    change,
                },
            )
        }
        FixtureControl::Reset => {
            let reset = ScopedAppendReset {
                old_generation: 1,
                new_generation: 2,
                reason: AppendTransition::PrefixMismatch,
            };
            let event_id = source_reset_event_id(&source, reset);
            (
                3,
                ScopedAppendDeliveryPhase::Correction,
                event_id,
                ScopedProjectedObservation::SourceReset {
                    object_token: 7,
                    source: source.clone(),
                    lane_ordinal: 3,
                    observed_at: 43,
                    phase: ScopedAppendDeliveryPhase::Correction,
                    event_id,
                    reset,
                },
            )
        }
        FixtureControl::RetryScheduled
        | FixtureControl::RetryExhausted
        | FixtureControl::TerminalError => {
            let terminal = matches!(control, FixtureControl::TerminalError);
            let exhausted = matches!(control, FixtureControl::RetryExhausted);
            let error = Arc::new(ScopedSourceObjectError {
                error_contract_version: SCOPED_SOURCE_OBJECT_ERROR_CONTRACT_VERSION,
                relation_id: Arc::from("root-object"),
                source: source.clone(),
                scope_epoch: 1,
                failure_code: if terminal {
                    ScopedSourceObjectFailureCode::DecodeStreamFatal
                } else {
                    ScopedSourceObjectFailureCode::DecodeRetryTransient
                },
                provenance: ScopedSourceObjectErrorProvenance {
                    generation: 2,
                    last_successful_position: (!terminal).then(|| {
                        CoveragePosition::derive(
                            CoveragePositionKind::AppendCursor,
                            b"append-offset-20",
                            Some(20),
                        )
                        .unwrap()
                    }),
                },
                retry: if terminal {
                    ScopedSourceObjectRetryState::NotRetryable { failed_attempts: 1 }
                } else if exhausted {
                    ScopedSourceObjectRetryState::RetryExhausted {
                        failed_attempts: 4,
                        max_attempts: 4,
                    }
                } else {
                    ScopedSourceObjectRetryState::RetryScheduled {
                        failed_attempts: 1,
                        max_attempts: 4,
                        retry_after_ms: 10,
                    }
                },
            });
            let event_id = source_object_error_event_id(&error);
            (
                if terminal {
                    6
                } else if exhausted {
                    5
                } else {
                    4
                },
                ScopedAppendDeliveryPhase::Live,
                event_id,
                ScopedProjectedObservation::SourceObjectError {
                    source: source.clone(),
                    observed_at: if terminal {
                        46
                    } else if exhausted {
                        45
                    } else {
                        44
                    },
                    event_id,
                    error,
                },
            )
        }
    };
    let delivered = ScopedDeliveredObservation {
        event_contract_version: selection.event_contract_version,
        observer_sequence,
        scope_epoch: 1,
        event_id,
        semantic_revision_ref: None,
        phase,
        source: source.clone(),
        event: projected,
    };
    let envelope = ScopedObservationEnvelopeMapper::new(root.clone(), selection.clone())
        .map(delivered)
        .unwrap();
    (envelope, selection, root, source)
}

fn wire(control: FixtureControl) -> ScopedSourceEnvelopeWire {
    let (envelope, _, root, source) = mapped_envelope(control);
    ScopedSourceEnvelopeWire::from_scoped_for_context(
        &envelope,
        &root,
        std::slice::from_ref(&source),
    )
    .unwrap()
}

fn parse_for_context(
    value: Value,
    selection: &ObservationContractSelection,
    root: &ScopedObservationRootIdentity,
    source: &ScopedSourceObjectIdentity,
) -> Result<ScopedSourceEnvelopeWire, ScopedSourceEnvelopeContractError> {
    ScopedSourceEnvelopeWire::from_wire_value_for_context(
        value,
        selection,
        root,
        std::slice::from_ref(source),
    )
}

fn fixture_value() -> Value {
    let (_, selection, root, source) = mapped_envelope(FixtureControl::Created);
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
        "created": wire(FixtureControl::Created),
        "deleted": wire(FixtureControl::Deleted),
        "reset": wire(FixtureControl::Reset),
        "retry_scheduled": wire(FixtureControl::RetryScheduled),
        "retry_exhausted": wire(FixtureControl::RetryExhausted),
        "terminal_error": wire(FixtureControl::TerminalError),
        "expected": {
            "complete_event_union": false,
            "supported_variants": [
                "source_created",
                "source_deleted",
                "source_reset",
                "source_object_error",
            ],
            "unsupported_variants": "observer_lifecycle_and_semantic_fact_events",
            "event_id_authority": "native_rust_contextual_parser",
            "native_evidence": "engine_control_only",
        },
    })
}

#[test]
fn all_source_control_variants_round_trip_only_with_exact_context() {
    for control in [
        FixtureControl::Created,
        FixtureControl::Deleted,
        FixtureControl::Reset,
        FixtureControl::RetryScheduled,
        FixtureControl::RetryExhausted,
        FixtureControl::TerminalError,
    ] {
        let (envelope, selection, root, source) = mapped_envelope(control);
        let wire = ScopedSourceEnvelopeWire::from_scoped_for_context(
            &envelope,
            &root,
            std::slice::from_ref(&source),
        )
        .unwrap();
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
fn selection_root_and_source_authority_cannot_be_retargeted() {
    let (_, selection, root, source) = mapped_envelope(FixtureControl::Created);
    let base = serde_json::to_value(wire(FixtureControl::Created)).unwrap();

    let mut wrong_selection = base.clone();
    wrong_selection["contract_selection"]["event_contract_version"] = json!(2);
    assert!(parse_for_context(wrong_selection, &selection, &root, &source).is_err());

    let mut wrong_root = root.clone();
    wrong_root.root_actor_run_key = CanonicalEntityKey::derive_root_actor_run(
        wrong_root.adapter_id.as_str(),
        &wrong_root.source_instance_key,
        &wrong_root.session_key,
        Some(b"other-root"),
    )
    .unwrap();
    assert!(parse_for_context(base.clone(), &selection, &wrong_root, &source).is_err());

    let mut foreign = source.clone();
    foreign.object_key = CoverageObjectKey::derive("fixture.other", b"other").unwrap();
    assert!(parse_for_context(base.clone(), &selection, &root, &foreign).is_err());
    assert!(ScopedSourceEnvelopeWire::from_wire_value_for_context(
        base.clone(),
        &selection,
        &root,
        &[],
    )
    .is_err());
    assert!(ScopedSourceEnvelopeWire::from_wire_value_for_context(
        base,
        &selection,
        &root,
        &vec![source.clone(); MAX_AUTHORIZED_SOURCES + 1],
    )
    .is_err());

    let base = serde_json::to_value(wire(FixtureControl::Created)).unwrap();
    assert!(ScopedSourceEnvelopeWire::from_wire_value_for_context(
        base.clone(),
        &selection,
        &root,
        &[source.clone(), source.clone()],
    )
    .is_err());
    let mut foreign_adapter = source.clone();
    foreign_adapter.adapter_id = AdapterId::new("other").unwrap();
    assert!(ScopedSourceEnvelopeWire::from_wire_value_for_context(
        base,
        &selection,
        &root,
        &[source, foreign_adapter],
    )
    .is_err());
}

#[test]
fn source_lineage_error_state_and_event_ids_are_recomputed() {
    let (_, selection, root, source) = mapped_envelope(FixtureControl::Reset);
    let reset = serde_json::to_value(wire(FixtureControl::Reset)).unwrap();
    let reject_reset = |value| {
        assert!(parse_for_context(value, &selection, &root, &source).is_err());
    };

    let mut event_id = reset.clone();
    event_id["event_id"] = json!(format!("v1:{}", "A".repeat(43)));
    reject_reset(event_id);

    let mut skipped_generation = reset.clone();
    skipped_generation["event"]["new_generation"] = json!(3);
    skipped_generation["source"]["generation"] = json!(3);
    reject_reset(skipped_generation);

    let mut wrong_phase = reset.clone();
    wrong_phase["phase"] = json!("live");
    reject_reset(wrong_phase);

    let (_, selection, root, source) = mapped_envelope(FixtureControl::RetryScheduled);
    let retry = serde_json::to_value(wire(FixtureControl::RetryScheduled)).unwrap();
    let reject_retry = |value| {
        assert!(parse_for_context(value, &selection, &root, &source).is_err());
    };

    let mut wrong_scope = retry.clone();
    wrong_scope["event"]["error"]["scope_epoch"] = json!(2);
    reject_retry(wrong_scope);

    let mut wrong_failure_class = retry.clone();
    wrong_failure_class["event"]["error"]["failure_code"] = json!("decode_stream_fatal");
    reject_retry(wrong_failure_class);

    let mut bad_attempts = retry.clone();
    bad_attempts["event"]["error"]["retry"]["failed_attempts"] = json!(4);
    reject_retry(bad_attempts);

    let mut unsafe_position = retry.clone();
    unsafe_position["event"]["error"]["provenance"]["last_successful_position"]
        ["monotonic_order"] = json!(JS_SAFE_INTEGER_MAX + 1);
    reject_retry(unsafe_position);

    let mut false_completeness = retry;
    false_completeness["evidence"]["completeness"] = json!("complete");
    reject_retry(false_completeness);
}

#[test]
fn controls_are_portably_bounded_and_cannot_disclose_record_occurrences() {
    let (_, selection, root, source) = mapped_envelope(FixtureControl::Created);
    let base = serde_json::to_value(wire(FixtureControl::Created)).unwrap();
    let reject = |value| {
        assert!(parse_for_context(value, &selection, &root, &source).is_err());
    };

    let mut zero_generation = base.clone();
    zero_generation["source"]["generation"] = json!(0);
    zero_generation["event"]["generation"] = json!(0);
    reject(zero_generation);

    let mut unsafe_sequence = base.clone();
    unsafe_sequence["observer_sequence"] = json!(JS_SAFE_INTEGER_MAX + 1);
    reject(unsafe_sequence);

    for field in [
        "locator_id",
        "source_record_id",
        "record_index",
        "cursor_start",
        "cursor_end",
        "byte_range",
    ] {
        let mut disclosed = base.clone();
        disclosed["source"][field] = json!(match field {
            "record_index" => json!(0),
            "byte_range" => json!({"start": 0, "end": 1}),
            _ => json!("native-value"),
        });
        reject(disclosed);
    }

    let mut semantic = base.clone();
    semantic["semantic_revision_ref"] = json!({
        "semantic_reference_contract_version": 1,
        "fact_revision_id": root.session_key,
    });
    reject(semantic);

    let mut native = base;
    native["native_evidence"] = json!({"kind": "inline_source_record"});
    reject(native);
}

#[test]
fn strict_nested_shapes_reject_unknown_or_missing_meaning() {
    let (_, selection, root, source) = mapped_envelope(FixtureControl::RetryScheduled);
    let base = serde_json::to_value(wire(FixtureControl::RetryScheduled)).unwrap();

    for path in [
        &[][..],
        &["root"][..],
        &["root", "native_session_claim", "identity"][..],
        &["actor_attribution"][..],
        &["source"][..],
        &["evidence"][..],
        &["event"][..],
        &["event", "error"][..],
        &["event", "error", "provenance"][..],
        &["event", "error", "provenance", "last_successful_position"][..],
        &["event", "error", "retry"][..],
        &["native_evidence"][..],
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
        &["semantic_revision_ref"][..],
        &["root", "native_session_claim"][..],
        &["actor", "parent_run_key"][..],
        &["source", "locator_id"][..],
        &["native_time"][..],
        &["evidence", "effective_at"][..],
        &["event", "error", "provenance", "last_successful_position"][..],
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
fn source_wire_rejects_semantic_and_observer_lifecycle_variants() {
    let (mut envelope, _, root, source) = mapped_envelope(FixtureControl::Created);
    envelope.event = ScopedObservationEvent::ObserverFailed {
        failure: Arc::new(ScopedObserverFailure {
            root: root.clone(),
            failed_scope_epoch: 1,
            control_sequence: 2,
            last_contiguous_sequence: 1,
            phase: ScopedAppendDeliveryPhase::Live,
            reason: ScopedObserverFailureReason::InternalControlFailure,
            discarded_semantic_events: 0,
            discarded_source_controls: 0,
            discarded_retained_native_bytes: 0,
        }),
    };
    assert_eq!(
        ScopedSourceEnvelopeWire::from_scoped_for_context(
            &envelope,
            &root,
            std::slice::from_ref(&source),
        )
        .unwrap_err(),
        ScopedSourceEnvelopeContractError::UnsupportedEvent
    );
}

#[test]
fn frozen_rust_source_envelope_fixture_is_stable() {
    let actual = fixture_value();
    let expected: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    if actual != expected {
        eprintln!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
    assert_eq!(actual, expected);
}
