use std::collections::BTreeMap;
use std::sync::Arc;

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
    include_str!("../../../fixtures/contracts/rfc012d-scoped-continuity-envelope-v1.json");

#[derive(Debug, Clone, Copy)]
enum FixtureControl {
    Required(ScopedResyncReason),
    Started,
    Failed {
        phase: ScopedAppendDeliveryPhase,
        reason: ScopedObserverFailureReason,
    },
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

fn baseline(label: &[u8]) -> ScopedReplacementSnapshotDigest {
    ScopedReplacementSnapshotDigest(*blake3::hash(label).as_bytes())
}

fn required_control(
    root: &ScopedObservationRootIdentity,
    reason: ScopedResyncReason,
) -> ScopedResyncRequired {
    ScopedResyncRequired {
        root: root.clone(),
        invalid_scope_epoch: 1,
        control_sequence: 4,
        last_contiguous_sequence: 1,
        baseline_snapshot_digest: baseline(b"epoch-1-baseline"),
        reason,
        discarded_semantic_events: 2,
        discarded_source_controls: 1,
        discarded_retained_native_bytes: 144,
    }
}

fn context_for(
    root: &ScopedObservationRootIdentity,
    control: FixtureControl,
) -> ScopedContinuityConsumerContext {
    match control {
        FixtureControl::Required(_) => ScopedContinuityConsumerContext {
            current_scope_epoch: 1,
            last_contiguous_sequence: 1,
            baseline_snapshot_digest: Some(baseline(b"epoch-1-baseline")),
            phase: ScopedAppendDeliveryPhase::Live,
            prior_resync_required: None,
        },
        FixtureControl::Started => ScopedContinuityConsumerContext {
            current_scope_epoch: 1,
            last_contiguous_sequence: 4,
            baseline_snapshot_digest: Some(baseline(b"epoch-1-baseline")),
            phase: ScopedAppendDeliveryPhase::Live,
            prior_resync_required: Some(required_control(
                root,
                ScopedResyncReason::WatcherOverflow,
            )),
        },
        FixtureControl::Failed { phase, .. } => ScopedContinuityConsumerContext {
            current_scope_epoch: if phase == ScopedAppendDeliveryPhase::Correction {
                2
            } else {
                1
            },
            last_contiguous_sequence: if phase == ScopedAppendDeliveryPhase::Bootstrap {
                0
            } else if phase == ScopedAppendDeliveryPhase::Correction {
                5
            } else {
                2
            },
            baseline_snapshot_digest: (phase != ScopedAppendDeliveryPhase::Bootstrap)
                .then(|| baseline(b"epoch-1-baseline")),
            phase,
            prior_resync_required: None,
        },
    }
}

fn mapped_envelope(
    control: FixtureControl,
) -> (
    ScopedObservationEnvelope,
    ObservationContractSelection,
    ScopedObservationRootIdentity,
    ScopedContinuityConsumerContext,
) {
    let selection = contract_selection();
    let root = root_identity();
    let source = observer_control_source(&root).unwrap();
    let context = context_for(&root, control);
    let (observer_sequence, scope_epoch, phase, event_id, projected) = match control {
        FixtureControl::Required(reason) => {
            let required = Arc::new(required_control(&root, reason));
            let event_id = resync_required_event_id(
                &root,
                required.invalid_scope_epoch,
                required.reason,
                required.baseline_snapshot_digest,
            );
            (
                required.control_sequence,
                required.invalid_scope_epoch,
                ScopedAppendDeliveryPhase::Live,
                event_id,
                ScopedProjectedObservation::ObserverResyncRequired {
                    source: source.clone(),
                    observed_at: 41,
                    event_id,
                    control: required,
                },
            )
        }
        FixtureControl::Started => {
            let prior = required_control(&root, ScopedResyncReason::WatcherOverflow);
            let started = Arc::new(ScopedResyncStarted {
                root: root.clone(),
                old_scope_epoch: 1,
                new_scope_epoch: 2,
                control_sequence: 5,
                required_control_sequence: prior.control_sequence,
                baseline_snapshot_digest: prior.baseline_snapshot_digest,
                reason: prior.reason,
                replacement: ScopedReplacementMode::FullSnapshot,
            });
            let event_id = resync_started_event_id(&root, &started);
            (
                started.control_sequence,
                started.new_scope_epoch,
                ScopedAppendDeliveryPhase::Correction,
                event_id,
                ScopedProjectedObservation::ObserverResyncStarted {
                    source: source.clone(),
                    observed_at: 42,
                    event_id,
                    control: started,
                },
            )
        }
        FixtureControl::Failed { phase, reason } => {
            let failure = Arc::new(ScopedObserverFailure {
                root: root.clone(),
                failed_scope_epoch: context.current_scope_epoch,
                control_sequence: context.last_contiguous_sequence + 3,
                last_contiguous_sequence: context.last_contiguous_sequence,
                phase,
                reason,
                discarded_semantic_events: 3,
                discarded_source_controls: 2,
                discarded_retained_native_bytes: 233,
            });
            let event_id =
                observer_failed_event_id(&root, failure.failed_scope_epoch, failure.reason);
            (
                failure.control_sequence,
                failure.failed_scope_epoch,
                phase,
                event_id,
                ScopedProjectedObservation::ObserverFailed {
                    source: source.clone(),
                    observed_at: 50 + failure.failed_scope_epoch as i64,
                    event_id,
                    failure,
                },
            )
        }
    };
    let delivered = ScopedDeliveredObservation {
        event_contract_version: selection.event_contract_version,
        observer_sequence,
        scope_epoch,
        event_id,
        semantic_revision_ref: None,
        phase,
        source,
        event: projected,
    };
    let envelope = ScopedObservationEnvelopeMapper::new(root.clone(), selection.clone())
        .map(delivered)
        .unwrap();
    (envelope, selection, root, context)
}

fn wire(control: FixtureControl) -> ScopedContinuityEnvelopeWire {
    let (envelope, selection, root, context) = mapped_envelope(control);
    ScopedContinuityEnvelopeWire::from_scoped_for_context(&envelope, &selection, &root, &context)
        .unwrap()
}

fn parse_for_context(
    value: Value,
    selection: &ObservationContractSelection,
    root: &ScopedObservationRootIdentity,
    context: &ScopedContinuityConsumerContext,
) -> Result<ScopedContinuityEnvelopeWire, ScopedContinuityEnvelopeContractError> {
    ScopedContinuityEnvelopeWire::from_wire_value_for_context(value, selection, root, context)
}

fn portable_context_value(
    selection: &ObservationContractSelection,
    root: &ScopedObservationRootIdentity,
    context: &ScopedContinuityConsumerContext,
) -> Value {
    let envelope_context = super::ScopedContinuityEnvelopeConsumerContext {
        attachment_authority: next_scoped_attachment_authority().unwrap(),
        contract_selection: selection.clone(),
        root: root.clone(),
        state: context.clone(),
    };
    serde_json::to_value(envelope_context.wire().unwrap()).unwrap()
}

fn fixture_value() -> Value {
    let required_kinds = [
        ScopedResyncReason::WatcherOverflow,
        ScopedResyncReason::TransportContinuityLoss,
        ScopedResyncReason::ExplicitConsumerRequest,
    ];
    let (_, selection, root, required_context) = mapped_envelope(FixtureControl::Required(
        ScopedResyncReason::WatcherOverflow,
    ));
    let (_, _, _, started_context) = mapped_envelope(FixtureControl::Started);
    let (_, _, _, bootstrap_failure_context) = mapped_envelope(FixtureControl::Failed {
        phase: ScopedAppendDeliveryPhase::Bootstrap,
        reason: ScopedObserverFailureReason::NativeWatcherRoutingFailed,
    });
    let (_, _, _, live_failure_context) = mapped_envelope(FixtureControl::Failed {
        phase: ScopedAppendDeliveryPhase::Live,
        reason: ScopedObserverFailureReason::NativeWatcherRecoveryExhausted,
    });
    let (_, _, _, correction_failure_context) = mapped_envelope(FixtureControl::Failed {
        phase: ScopedAppendDeliveryPhase::Correction,
        reason: ScopedObserverFailureReason::InternalControlFailure,
    });
    json!({
        "fixture_contract_version": 1,
        "contexts": {
            "required": portable_context_value(&selection, &root, &required_context),
            "started": portable_context_value(&selection, &root, &started_context),
            "failed_bootstrap": portable_context_value(&selection, &root, &bootstrap_failure_context),
            "failed_live": portable_context_value(&selection, &root, &live_failure_context),
            "failed_correction": portable_context_value(&selection, &root, &correction_failure_context),
        },
        "resync_required": {
            "watcher_overflow": wire(FixtureControl::Required(required_kinds[0])),
            "transport_continuity_loss": wire(FixtureControl::Required(required_kinds[1])),
            "explicit_consumer_request": wire(FixtureControl::Required(required_kinds[2])),
        },
        "resync_started": wire(FixtureControl::Started),
        "observer_failed": {
            "bootstrap": wire(FixtureControl::Failed {
                phase: ScopedAppendDeliveryPhase::Bootstrap,
                reason: ScopedObserverFailureReason::NativeWatcherRoutingFailed,
            }),
            "live": wire(FixtureControl::Failed {
                phase: ScopedAppendDeliveryPhase::Live,
                reason: ScopedObserverFailureReason::NativeWatcherRecoveryExhausted,
            }),
            "correction": wire(FixtureControl::Failed {
                phase: ScopedAppendDeliveryPhase::Correction,
                reason: ScopedObserverFailureReason::InternalControlFailure,
            }),
        },
        "expected": {
            "complete_event_union": false,
            "supported_variants": [
                "observer_resync_required",
                "observer_resync_started",
                "observer_failed",
            ],
            "unsupported_variants": "bootstrap_and_resync_completion_barriers_semantic_events_close_and_future_unknowns",
            "event_id_authority": "native_rust_contextual_parser",
            "diagnostic_discard_counts_in_event_identity": false,
            "replacement": "full_snapshot_only",
        },
    })
}

#[test]
fn every_frozen_continuity_control_round_trips_only_with_caller_state() {
    for control in [
        FixtureControl::Required(ScopedResyncReason::WatcherOverflow),
        FixtureControl::Required(ScopedResyncReason::TransportContinuityLoss),
        FixtureControl::Required(ScopedResyncReason::ExplicitConsumerRequest),
        FixtureControl::Started,
        FixtureControl::Failed {
            phase: ScopedAppendDeliveryPhase::Bootstrap,
            reason: ScopedObserverFailureReason::NativeWatcherRoutingFailed,
        },
        FixtureControl::Failed {
            phase: ScopedAppendDeliveryPhase::Live,
            reason: ScopedObserverFailureReason::NativeWatcherRecoveryExhausted,
        },
        FixtureControl::Failed {
            phase: ScopedAppendDeliveryPhase::Correction,
            reason: ScopedObserverFailureReason::InternalControlFailure,
        },
    ] {
        let (envelope, selection, root, context) = mapped_envelope(control);
        let wire = ScopedContinuityEnvelopeWire::from_scoped_for_context(
            &envelope, &selection, &root, &context,
        )
        .unwrap();
        let parsed = parse_for_context(
            serde_json::to_value(&wire).unwrap(),
            &selection,
            &root,
            &context,
        )
        .unwrap();
        assert_eq!(parsed, wire);
    }
}

#[test]
fn invalidation_binds_epoch_watermark_baseline_and_reason_identity() {
    let control = FixtureControl::Required(ScopedResyncReason::WatcherOverflow);
    let (_, selection, root, context) = mapped_envelope(control);
    let base = serde_json::to_value(wire(control)).unwrap();
    let reject = |value, state: &ScopedContinuityConsumerContext| {
        assert!(parse_for_context(value, &selection, &root, state).is_err());
    };

    let mut wrong_epoch = context.clone();
    wrong_epoch.current_scope_epoch = 2;
    reject(base.clone(), &wrong_epoch);

    let mut wrong_watermark = context.clone();
    wrong_watermark.last_contiguous_sequence = 2;
    reject(base.clone(), &wrong_watermark);

    let mut wrong_baseline = context.clone();
    wrong_baseline.baseline_snapshot_digest = Some(baseline(b"other-baseline"));
    reject(base.clone(), &wrong_baseline);

    let mut forged_id = base.clone();
    forged_id["event_id"] = json!(encode_opaque(&[7; 32]));
    reject(forged_id, &context);

    let mut false_phase = base;
    false_phase["phase"] = json!("correction");
    reject(false_phase, &context);

    let (_, _, _, already_invalidated) = mapped_envelope(FixtureControl::Started);
    let mut duplicate_invalidation = serde_json::to_value(wire(FixtureControl::Required(
        ScopedResyncReason::WatcherOverflow,
    )))
    .unwrap();
    duplicate_invalidation["observer_sequence"] = json!(5);
    duplicate_invalidation["event"]["control_sequence"] = json!(5);
    duplicate_invalidation["event"]["last_contiguous_sequence"] = json!(4);
    reject(duplicate_invalidation, &already_invalidated);
}

#[test]
fn replacement_start_requires_the_exact_delivered_invalidation() {
    let (_, selection, root, context) = mapped_envelope(FixtureControl::Started);
    let base = serde_json::to_value(wire(FixtureControl::Started)).unwrap();
    let reject = |value, state: &ScopedContinuityConsumerContext| {
        assert!(parse_for_context(value, &selection, &root, state).is_err());
    };

    let mut absent = context.clone();
    absent.prior_resync_required = None;
    reject(base.clone(), &absent);

    let mut not_delivered = context.clone();
    not_delivered.last_contiguous_sequence = 1;
    reject(base.clone(), &not_delivered);

    let mut reason_drift = context.clone();
    reason_drift.prior_resync_required.as_mut().unwrap().reason =
        ScopedResyncReason::TransportContinuityLoss;
    reject(base.clone(), &reason_drift);

    let mut sequence_drift = base.clone();
    sequence_drift["event"]["required_control_sequence"] = json!(3);
    reject(sequence_drift, &context);

    let mut skipped_epoch = base.clone();
    skipped_epoch["scope_epoch"] = json!(3);
    skipped_epoch["source"]["generation"] = json!(3);
    skipped_epoch["event"]["new_scope_epoch"] = json!(3);
    reject(skipped_epoch, &context);

    let mut partial = base;
    partial["event"]["replacement"] = json!("incremental");
    reject(partial, &context);
}

#[test]
fn terminal_failure_binds_caller_epoch_watermark_and_phase() {
    let control = FixtureControl::Failed {
        phase: ScopedAppendDeliveryPhase::Correction,
        reason: ScopedObserverFailureReason::InternalControlFailure,
    };
    let (_, selection, root, context) = mapped_envelope(control);
    let base = serde_json::to_value(wire(control)).unwrap();
    let reject = |value, state: &ScopedContinuityConsumerContext| {
        assert!(parse_for_context(value, &selection, &root, state).is_err());
    };

    let mut live = context.clone();
    live.phase = ScopedAppendDeliveryPhase::Live;
    reject(base.clone(), &live);

    let mut wrong_epoch = context.clone();
    wrong_epoch.current_scope_epoch = 1;
    reject(base.clone(), &wrong_epoch);

    let mut wrong_watermark = base.clone();
    wrong_watermark["event"]["last_contiguous_sequence"] = json!(4);
    reject(wrong_watermark, &context);

    let mut wrong_event_phase = base;
    wrong_event_phase["event"]["phase"] = json!("live");
    reject(wrong_event_phase, &context);
}

#[test]
fn counters_and_control_coordinate_are_portably_bounded() {
    let control = FixtureControl::Required(ScopedResyncReason::WatcherOverflow);
    let (_, selection, root, context) = mapped_envelope(control);
    let base = serde_json::to_value(wire(control)).unwrap();
    let reject = |value| {
        assert!(parse_for_context(value, &selection, &root, &context).is_err());
    };

    for path in [
        &["observer_sequence"][..],
        &["event", "discarded_semantic_events"][..],
        &["event", "discarded_source_controls"][..],
        &["event", "discarded_retained_native_bytes"][..],
    ] {
        let mut unsafe_value = base.clone();
        let mut target = &mut unsafe_value;
        for segment in &path[..path.len() - 1] {
            target = &mut target[*segment];
        }
        target[path[path.len() - 1]] = json!(JS_SAFE_INTEGER_MAX + 1);
        reject(unsafe_value);
    }

    let mut zero_epoch = base.clone();
    zero_epoch["scope_epoch"] = json!(0);
    zero_epoch["source"]["generation"] = json!(0);
    zero_epoch["event"]["invalid_scope_epoch"] = json!(0);
    reject(zero_epoch);

    let mut disclosed = base.clone();
    disclosed["source"]["locator_id"] = json!("/native/path");
    reject(disclosed);

    let mut foreign_control_source = base;
    foreign_control_source["source"]["object_key"] = json!(root.session_key);
    reject(foreign_control_source);
}

#[test]
fn strict_nested_shapes_never_drop_unknown_or_missing_meaning() {
    let control = FixtureControl::Started;
    let (_, selection, root, context) = mapped_envelope(control);
    let base = serde_json::to_value(wire(control)).unwrap();

    for path in [
        &[][..],
        &["root"][..],
        &["root", "native_session_claim", "identity"][..],
        &["actor"][..],
        &["actor_attribution"][..],
        &["affiliations"][..],
        &["source"][..],
        &["evidence"][..],
        &["event"][..],
        &["native_evidence"][..],
    ] {
        let mut unknown = base.clone();
        let mut target = &mut unknown;
        for segment in path {
            target = &mut target[*segment];
        }
        target["future_meaning"] = json!(true);
        assert!(parse_for_context(unknown, &selection, &root, &context).is_err());
    }

    for path in [
        &["semantic_revision_ref"][..],
        &["root", "native_session_claim"][..],
        &["actor", "parent_run_key"][..],
        &["source", "locator_id"][..],
        &["native_time"][..],
        &["evidence", "effective_at"][..],
    ] {
        let mut missing = base.clone();
        let (field, parents) = path.split_last().unwrap();
        let mut target = &mut missing;
        for segment in parents {
            target = &mut target[*segment];
        }
        target.as_object_mut().unwrap().remove(*field);
        assert!(parse_for_context(missing, &selection, &root, &context).is_err());
    }
}

#[test]
fn this_slice_rejects_barriers_semantic_events_and_source_controls() {
    let (mut envelope, selection, root, context) = mapped_envelope(FixtureControl::Started);
    envelope.event = ScopedObservationEvent::SourcePresence {
        change: ScopedAppendPresenceChange::Created { generation: 2 },
    };
    assert_eq!(
        ScopedContinuityEnvelopeWire::from_scoped_for_context(
            &envelope, &selection, &root, &context,
        ),
        Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent)
    );
}

#[test]
fn serializer_cannot_sanitize_mismatched_mapped_envelope_fields() {
    let (envelope, selection, root, context) = mapped_envelope(FixtureControl::Started);

    let mut wrong_selection = envelope.clone();
    wrong_selection
        .contract_selection
        .lifecycle_contract_version = 2;
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &wrong_selection,
        &selection,
        &root,
        &context,
    )
    .is_err());

    let mut wrong_root = envelope.clone();
    wrong_root.root.root_actor_run_key = root.session_key;
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &wrong_root,
        &selection,
        &root,
        &context,
    )
    .is_err());

    let mut wrong_actor = envelope.clone();
    wrong_actor.actor.run_key = root.session_key;
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &wrong_actor,
        &selection,
        &root,
        &context,
    )
    .is_err());

    let mut wrong_source = envelope.clone();
    wrong_source.source.object_key = CoverageObjectKey::derive("fixture.other", b"other").unwrap();
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &wrong_source,
        &selection,
        &root,
        &context,
    )
    .is_err());

    let mut wrong_control = envelope;
    let ScopedObservationEvent::ObserverResyncStarted { control } = &wrong_control.event else {
        panic!("fixture must be resync-started");
    };
    let mut control = (**control).clone();
    control.root.root_actor_run_key = root.session_key;
    wrong_control.event = ScopedObservationEvent::ObserverResyncStarted {
        control: Arc::new(control),
    };
    assert_eq!(
        ScopedContinuityEnvelopeWire::from_scoped_for_context(
            &wrong_control,
            &selection,
            &root,
            &context,
        ),
        Err(ScopedContinuityEnvelopeContractError::UnsupportedEvent)
    );
}

#[test]
fn baseline_authority_is_absent_only_before_the_first_completed_snapshot() {
    let (bootstrap_failure, selection, root, bootstrap_context) =
        mapped_envelope(FixtureControl::Failed {
            phase: ScopedAppendDeliveryPhase::Bootstrap,
            reason: ScopedObserverFailureReason::NativeWatcherRoutingFailed,
        });
    assert_eq!(bootstrap_context.baseline_snapshot_digest, None);
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &bootstrap_failure,
        &selection,
        &root,
        &bootstrap_context,
    )
    .is_ok());
    assert_eq!(
        portable_context_value(&selection, &root, &bootstrap_context)["state"]
            ["baseline_snapshot_digest"],
        Value::Null
    );
    let mut invented_bootstrap_baseline = bootstrap_context.clone();
    invented_bootstrap_baseline.baseline_snapshot_digest =
        Some(baseline(b"invented-bootstrap-baseline"));
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &bootstrap_failure,
        &selection,
        &root,
        &invented_bootstrap_baseline,
    )
    .is_err());

    let (live_failure, selection, root, mut live_context) =
        mapped_envelope(FixtureControl::Failed {
            phase: ScopedAppendDeliveryPhase::Live,
            reason: ScopedObserverFailureReason::NativeWatcherRoutingFailed,
        });
    live_context.baseline_snapshot_digest = None;
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &live_failure,
        &selection,
        &root,
        &live_context,
    )
    .is_err());

    let (required, selection, root, mut required_context) = mapped_envelope(
        FixtureControl::Required(ScopedResyncReason::WatcherOverflow),
    );
    required_context.baseline_snapshot_digest = None;
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &required,
        &selection,
        &root,
        &required_context,
    )
    .is_err());

    let mut wrong_phase = context_for(
        &root,
        FixtureControl::Required(ScopedResyncReason::WatcherOverflow),
    );
    wrong_phase.phase = ScopedAppendDeliveryPhase::Correction;
    assert!(ScopedContinuityEnvelopeWire::from_scoped_for_context(
        &required,
        &selection,
        &root,
        &wrong_phase,
    )
    .is_err());
}

#[test]
fn frozen_rust_continuity_fixture_is_stable() {
    let expected: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    assert_eq!(fixture_value(), expected);
}
