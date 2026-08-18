use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, ContractCompleteness, ContractVersionOffer, ContractVersionRequest,
    FactSemanticContext, NativeIdentity, QualifiedValue, QualifiedValueQuality,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
};

use super::super::*;
use super::*;

const FROZEN_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-close-v1.json");

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

fn root_identity() -> ScopedObservationRootIdentity {
    let adapter_id = AdapterId::new("fixture").unwrap();
    let context = FactSemanticContext::new(
        &adapter_id,
        1,
        b"stable-source-instance",
        b"transcript",
        b"root-session.jsonl",
        1,
    )
    .unwrap();
    let session_key = CanonicalEntityKey::derive(
        adapter_id.as_str(),
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
    .resolve(&adapter_id, 1)
    .unwrap()
}

fn command(token: u64) -> ScopedCloseCommand {
    ScopedCloseCommand::new(
        Arc::new(ScopedObservationAttachmentAuthority { token }),
        contract_selection(),
        root_identity(),
    )
    .unwrap()
}

fn completed_operation(token: u64) -> ScopedObservationCloseOperation {
    let lifecycle = Arc::new(ScopedObservationAttachmentLifecycle::default());
    let completion = Arc::new(ScopedObservationEventCompletion::default());
    lifecycle.open_consumer_drain(&completion).unwrap();
    let barrier = lifecycle.begin_close();
    lifecycle.close_consumer_drain();
    ScopedObservationCloseOperation {
        command: command(token),
        barrier,
    }
}

fn complete_state() -> ScopedObservationCloseState {
    ScopedObservationCloseState {
        close_requested: true,
        active_operations: 0,
        active_watcher_tasks: 0,
        consumer_drain_pending: false,
        complete: true,
    }
}

fn fixture_value() -> Value {
    let operation = completed_operation(41);
    let receipt = operation.receipt_if_complete().unwrap();
    json!({
        "context": operation.command.context_wire(),
        "receipt": receipt,
    })
}

fn mutate_receipt(
    mutator: impl FnOnce(&mut Value),
) -> Result<ScopedCloseReceiptWire, ScopedCloseContractError> {
    let operation = completed_operation(41);
    let mut value = serde_json::to_value(operation.receipt_if_complete().unwrap()).unwrap();
    mutator(&mut value);
    ScopedCloseReceiptWire::from_wire_value_for_operation(value, &operation)
}

#[test]
fn frozen_rust_close_receipt_fixture_is_stable() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    let operation = completed_operation(41);
    let parsed = ScopedCloseReceiptWire::from_wire_value_for_operation(
        fixture["receipt"].clone(),
        &operation,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), fixture["receipt"]);
}

#[test]
fn close_receipt_waits_for_watcher_and_consumer_acknowledgements() {
    let lifecycle = Arc::new(ScopedObservationAttachmentLifecycle::default());
    let completion = Arc::new(ScopedObservationEventCompletion::default());
    lifecycle.open_consumer_drain(&completion).unwrap();
    let watcher = lifecycle
        .start_operation(ScopedObservationOperationKind::Watcher)
        .unwrap();
    let operation = ScopedObservationCloseOperation {
        command: command(42),
        barrier: lifecycle.begin_close(),
    };

    assert_eq!(
        operation.receipt_if_complete(),
        Err(ScopedCloseContractError::NotComplete)
    );
    let forged_early = serde_json::to_value(
        ScopedCloseReceiptWire::from_completed_command(&operation.command, complete_state())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        ScopedCloseReceiptWire::from_wire_value_for_operation(forged_early, &operation),
        Err(ScopedCloseContractError::NotComplete)
    );
    let closing = operation.state();
    assert!(closing.close_requested);
    assert_eq!(closing.active_operations, 1);
    assert_eq!(closing.active_watcher_tasks, 1);
    assert!(closing.consumer_drain_pending);
    assert!(!closing.complete);

    lifecycle.close_consumer_drain();
    assert_eq!(
        operation.receipt_if_complete(),
        Err(ScopedCloseContractError::NotComplete)
    );
    drop(watcher);
    assert!(operation.receipt_if_complete().is_ok());
    assert!(matches!(
        lifecycle.start_operation(ScopedObservationOperationKind::Runtime),
        Err(ScopedObservationOperationStartError::Closing)
    ));
}

#[tokio::test]
async fn async_close_retains_completion_before_first_poll_and_is_idempotent() {
    let lifecycle = Arc::new(ScopedObservationAttachmentLifecycle::default());
    let first = command(43);
    let second = ScopedCloseCommand::new(
        Arc::clone(&first.attachment_authority),
        first.contract_selection.clone(),
        first.root.clone(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(first.context_wire()).unwrap(),
        serde_json::to_value(second.context_wire()).unwrap()
    );

    let operation = ScopedObservationCloseOperation {
        command: first.clone(),
        barrier: lifecycle.begin_close(),
    };
    let pending = operation.wait_async();
    let receipt = pending.await.unwrap();
    let repeated = operation.wait_async().await.unwrap();
    assert_eq!(receipt, repeated);
    assert!(ScopedCloseReceiptWire::from_wire_value_for_operation(
        serde_json::to_value(receipt).unwrap(),
        &operation,
    )
    .is_ok());
}

#[test]
fn completed_state_must_be_exact_and_internal_counts_never_serialize() {
    let command = command(44);
    let mut invalid = complete_state();
    invalid.active_operations = 1;
    assert_eq!(
        ScopedCloseReceiptWire::from_completed_command(&command, invalid),
        Err(ScopedCloseContractError::NotComplete)
    );
    invalid = complete_state();
    invalid.active_watcher_tasks = 1;
    assert_eq!(
        ScopedCloseReceiptWire::from_completed_command(&command, invalid),
        Err(ScopedCloseContractError::NotComplete)
    );
    invalid = complete_state();
    invalid.consumer_drain_pending = true;
    assert_eq!(
        ScopedCloseReceiptWire::from_completed_command(&command, invalid),
        Err(ScopedCloseContractError::NotComplete)
    );

    let serialized = serde_json::to_value(
        ScopedCloseReceiptWire::from_completed_command(&command, complete_state()).unwrap(),
    )
    .unwrap();
    let object = serialized.as_object().unwrap();
    assert!(!object.contains_key("active_operations"));
    assert!(!object.contains_key("active_watcher_tasks"));
    assert!(!object.contains_key("consumer_drain_pending"));
    assert!(!object.contains_key("applied_through_sequence"));
    assert!(!object.contains_key("observed_at"));
}

#[test]
fn receipt_consumption_rejects_foreign_attachment_and_every_context_drift() {
    let expected = completed_operation(45);
    let receipt = expected.receipt_if_complete().unwrap();
    let value = serde_json::to_value(&receipt).unwrap();
    let foreign = completed_operation(46);
    assert!(matches!(
        ScopedCloseReceiptWire::from_wire_value_for_operation(value.clone(), &foreign),
        Err(ScopedCloseContractError::Invalid { .. })
    ));

    for field in ["attachment_ref", "close_request_id"] {
        assert!(mutate_receipt(|value| {
            value[field] = json!(encode_opaque(&[9; DIGEST_BYTES]));
        })
        .is_err());
    }
    assert!(mutate_receipt(|value| value["outcome"] = json!("closing")).is_err());
    assert!(mutate_receipt(|value| {
        value["contract_selection"]["lifecycle_contract_version"] = json!(2);
    })
    .is_err());
    assert!(mutate_receipt(|value| {
        value["root"]["session_key"] = json!(encode_opaque(&[7; DIGEST_BYTES]));
    })
    .is_err());
    assert!(mutate_receipt(|value| {
        value["root"]["adapter_id"] = json!("other");
    })
    .is_err());
}

#[test]
fn receipt_wire_is_strict_and_has_no_unbound_deserialize_path() {
    assert!(mutate_receipt(|value| value["future"] = json!(true)).is_err());
    assert!(mutate_receipt(|value| {
        value.as_object_mut().unwrap().remove("close_request_id");
    })
    .is_err());
    assert!(mutate_receipt(|value| {
        value["root"]
            .as_object_mut()
            .unwrap()
            .remove("native_session_claim");
    })
    .is_err());
    assert!(mutate_receipt(|value| value["root"]["native_session_claim"] = json!({})).is_err());
    assert!(mutate_receipt(|value| {
        value["root"]["native_session_claim"]["future"] = json!(true);
    })
    .is_err());
    assert!(mutate_receipt(|value| {
        value["root"]["native_session_claim"]["identity"]["future"] = json!(true);
    })
    .is_err());
    assert!(mutate_receipt(|value| {
        value["root"]["native_session_claim"]["identity"]["value"]["future"] = json!(true);
    })
    .is_err());
    for optional in ["unknown_reason", "effective_at"] {
        assert!(mutate_receipt(|value| {
            value["root"]["native_session_claim"]["identity"][optional] = Value::Null;
        })
        .is_err());
    }
    assert!(mutate_receipt(|value| value["attachment_ref"] = json!("v1:AAAA=")).is_err());
    assert!(mutate_receipt(|value| value["close_request_id"] = json!("v1:AA")).is_err());
    assert!(mutate_receipt(|value| {
        let mut reference = value["attachment_ref"].as_str().unwrap().to_owned();
        reference.pop();
        reference.push('1');
        value["attachment_ref"] = json!(reference);
    })
    .is_err());
}

#[test]
fn attachment_ref_and_request_identity_cover_selection_root_and_attachment() {
    let base = command(47);
    let other_attachment = command(48);
    assert_ne!(base.attachment_ref, other_attachment.attachment_ref);
    assert_ne!(base.close_request_id, other_attachment.close_request_id);

    let mut other_root = root_identity();
    other_root.root_actor_run_key = CanonicalEntityKey::derive(
        "fixture",
        &other_root.source_instance_key,
        "actor-run",
        b"other-run",
    )
    .unwrap();
    let root_changed = ScopedCloseCommand::new(
        Arc::clone(&base.attachment_authority),
        base.contract_selection.clone(),
        other_root,
    )
    .unwrap();
    assert_ne!(base.attachment_ref, root_changed.attachment_ref);

    let mut other_selection = contract_selection();
    other_selection.contract_versions.fact_family_versions =
        BTreeMap::from([("runtime.usage-v2".to_owned(), 2)]);
    let selection_changed = ScopedCloseCommand::new(
        Arc::clone(&base.attachment_authority),
        other_selection,
        root_identity(),
    )
    .unwrap();
    assert_ne!(base.attachment_ref, selection_changed.attachment_ref);
}

#[test]
fn zero_attachment_token_is_rejected() {
    assert!(matches!(
        ScopedCloseCommand::new(
            Arc::new(ScopedObservationAttachmentAuthority { token: 0 }),
            contract_selection(),
            root_identity(),
        ),
        Err(ScopedCloseContractError::Invalid { .. })
    ));
}
