use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value as JsonValue};

use super::*;
use crate::adapter::{
    ContractVersionOffer, ContractVersionRequest, CONTRACT_VERSION_SELECTION_VERSION,
    EXTERNAL_ENTITY_REFERENCE_VERSION, SEMANTIC_REFERENCE_CONTRACT_VERSION,
    SOURCE_COVERAGE_CONTRACT_VERSION,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
    OBSERVATION_BASE_MODEL_MAJOR, OBSERVATION_PROFILE_CONTRACT_VERSION,
};

const USAGE_FAMILY: &str = "runtime.usage-v2";
const SOURCE_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-source-envelope-v1.json");
const FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-observation-unknown-wire-v1.json");

fn observation_selection() -> ObservationContractSelection {
    let request = ObservationContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: OBSERVATION_BASE_MODEL_MAJOR,
            external_entity_reference_version: EXTERNAL_ENTITY_REFERENCE_VERSION,
            semantic_revision_reference_version: SEMANTIC_REFERENCE_CONTRACT_VERSION,
            coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions: BTreeMap::from([(USAGE_FAMILY.to_owned(), vec![1])]),
            query_pack_versions: None,
            observation_contract_versions: Some(vec![OBSERVATION_PROFILE_CONTRACT_VERSION]),
        },
        vec![1],
        vec![1],
        vec![1],
    )
    .unwrap();
    let offer = ObservationContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: OBSERVATION_BASE_MODEL_MAJOR,
            external_entity_reference_versions: vec![EXTERNAL_ENTITY_REFERENCE_VERSION],
            semantic_revision_reference_versions: vec![SEMANTIC_REFERENCE_CONTRACT_VERSION],
            coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions: BTreeMap::from([(USAGE_FAMILY.to_owned(), vec![1])]),
            query_pack_versions: vec![],
            observation_contract_versions: vec![OBSERVATION_PROFILE_CONTRACT_VERSION],
        },
        vec![1],
        vec![1],
        vec![1],
    )
    .unwrap();
    negotiate_observation_contract(&request, &offer).unwrap()
}

fn request(max_preserved_bytes: u32) -> ObservationUnknownWireContractRequest {
    ObservationUnknownWireContractRequest::new(
        ObservationUnknownWireCapability::preserving(max_preserved_bytes).unwrap(),
    )
    .unwrap()
}

fn offer(max_preserved_bytes: u32) -> ObservationUnknownWireContractOffer {
    ObservationUnknownWireContractOffer::new(
        ObservationUnknownWireCapability::preserving(max_preserved_bytes).unwrap(),
    )
    .unwrap()
}

fn selection() -> ObservationUnknownWireContractSelection {
    negotiate_observation_unknown_wire(&request(8_192), &offer(4_096), &observation_selection())
        .unwrap()
}

fn carrier_wire() -> JsonValue {
    let source: JsonValue = serde_json::from_str(SOURCE_FIXTURE).unwrap();
    json!({
        "unknown_wire_event_contract_version": 1,
        "family": "unknown_wire_event",
        "type_tag": "runtime.message_delta_v2",
        "encoded_value": {
            "kind": "message_delta_v2",
            "segments": [
                {"format": "text", "ordinal": 1},
                {"format": "future_structured", "ordinal": 2}
            ]
        },
        "envelope_provenance": {
            "observation_selection": observation_selection(),
            "observer_sequence": source["created"]["observer_sequence"],
            "scope_epoch": source["created"]["scope_epoch"],
            "event_id": source["created"]["event_id"],
            "semantic_revision_ref": null,
            "source": {
                "instance_key": source["created"]["source"]["instance_key"],
                "stream_key": source["created"]["source"]["stream_key"],
                "object_key": source["created"]["source"]["object_key"],
                "generation": source["created"]["source"]["generation"]
            },
            "observed_at": source["created"]["observed_at"],
            "phase": source["created"]["phase"],
            "additional_envelope_provenance": {
                "actor_attribution": source["created"]["actor_attribution"],
                "evidence": source["created"]["evidence"]
            }
        }
    })
}

#[derive(Serialize)]
struct FixtureExpected {
    selected_max_preserved_bytes: u32,
    payload_depth_limit: usize,
    payload_node_limit: usize,
    known_event_type_tags: &'static [&'static str],
    runtime_unknown_emission: &'static str,
    native_transport: bool,
}

#[derive(Serialize)]
struct Fixture {
    fixture_contract_version: u32,
    contract_request: ObservationUnknownWireContractRequest,
    contract_offer: ObservationUnknownWireContractOffer,
    contract_selection: ObservationUnknownWireContractSelection,
    unknown_wire_event: JsonValue,
    expected: FixtureExpected,
}

#[test]
fn sidecar_negotiation_and_carrier_match_the_frozen_portable_fixture() {
    let selection = selection();
    let event = ObservationUnknownWireEvent::from_wire_value(carrier_wire(), &selection).unwrap();
    let actual = serde_json::to_value(Fixture {
        fixture_contract_version: 1,
        contract_request: request(8_192),
        contract_offer: offer(4_096),
        contract_selection: selection.clone(),
        unknown_wire_event: event.wire_value(),
        expected: FixtureExpected {
            selected_max_preserved_bytes: 4_096,
            payload_depth_limit: MAX_UNKNOWN_WIRE_DEPTH,
            payload_node_limit: MAX_UNKNOWN_WIRE_NODES,
            known_event_type_tags: KNOWN_EVENT_TYPE_TAGS,
            runtime_unknown_emission: "attachment_bound_internal",
            native_transport: false,
        },
    })
    .unwrap();
    let expected: JsonValue = serde_json::from_str(FIXTURE).unwrap();
    if actual != expected {
        panic!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
    assert_eq!(event.type_tag(), "runtime.message_delta_v2");
    assert_eq!(event.provenance().observer_sequence, 1);
    let debug = format!("{event:?}");
    assert!(!debug.contains("future_structured"));
}

#[test]
fn runtime_payload_binds_delivery_without_exposing_or_reparsing_authority() {
    let selected = Arc::new(selection());
    let payload = ObservationUnknownWireRuntimePayload::new(
        Arc::clone(&selected),
        "runtime.message_delta_v2".to_owned(),
        carrier_wire()["encoded_value"].clone(),
        carrier_wire()["envelope_provenance"]["additional_envelope_provenance"].clone(),
    )
    .unwrap();
    let source: JsonValue = serde_json::from_str(SOURCE_FIXTURE).unwrap();
    let instance_key =
        serde_json::from_value(source["created"]["source"]["instance_key"].clone()).unwrap();
    let stream_key =
        serde_json::from_value(source["created"]["source"]["stream_key"].clone()).unwrap();
    let object_key =
        serde_json::from_value(source["created"]["source"]["object_key"].clone()).unwrap();
    let event = ObservationUnknownWireEvent::from_runtime_payload(
        &payload,
        7,
        3,
        &[9; DIGEST_BYTES],
        None,
        instance_key,
        stream_key,
        object_key,
        2,
        44,
        "live",
    )
    .unwrap();

    assert!(payload.belongs_to_selection(&selected));
    assert!(event.belongs_to_runtime_selection(&selected));
    assert_eq!(event.encoded_bytes(), payload.encoded_bytes());
    assert_eq!(
        event.wire_value()["envelope_provenance"]["observer_sequence"],
        7
    );
    assert_eq!(event.wire_value()["envelope_provenance"]["scope_epoch"], 3);
    assert_eq!(event.wire_value()["envelope_provenance"]["phase"], "live");
    assert!(!format!("{payload:?}").contains("future_structured"));

    let equal_but_foreign = Arc::new((*selected).clone());
    assert!(!payload.belongs_to_selection(&equal_but_foreign));
    assert!(!event.belongs_to_runtime_selection(&equal_but_foreign));
    assert!(ObservationUnknownWireEvent::from_runtime_payload(
        &payload,
        7,
        3,
        &[9; DIGEST_BYTES],
        None,
        instance_key,
        stream_key,
        object_key,
        2,
        44,
        "future",
    )
    .is_err());
}

#[test]
fn selection_is_exactly_bound_to_both_capabilities_and_the_base_selection() {
    let base = observation_selection();
    let request = request(8_192);
    let offer = offer(4_096);
    let selected = negotiate_observation_unknown_wire(&request, &offer, &base).unwrap();
    assert_eq!(selected.capability.max_preserved_bytes, 4_096);
    let wire = serde_json::to_value(&selected).unwrap();
    assert_eq!(
        ObservationUnknownWireContractSelection::from_wire_value_for_negotiation(
            wire.clone(),
            &request,
            &offer,
            &base,
        )
        .unwrap(),
        selected
    );

    let mut drifted = wire.clone();
    drifted["capability"]["max_preserved_bytes"] = json!(8_192);
    assert!(
        ObservationUnknownWireContractSelection::from_wire_value_for_negotiation(
            drifted, &request, &offer, &base,
        )
        .is_err()
    );

    let mut base_drift = wire;
    base_drift["observation_selection"]["event_contract_version"] = json!(2);
    assert!(
        ObservationUnknownWireContractSelection::from_wire_value_for_observation_selection(
            base_drift, &base,
        )
        .is_err()
    );

    assert_eq!(
        ObservationUnknownWireContractSelection::from_wire_value_for_expected(
            serde_json::to_value(&selected).unwrap(),
            &selected,
        )
        .unwrap(),
        selected
    );

    for (field, expected_axis) in [
        (
            "preserves_type_tag",
            ObservationUnknownWireCompatibilityAxis::TypeTagPreservation,
        ),
        (
            "preserves_encoded_value",
            ObservationUnknownWireCompatibilityAxis::EncodedValuePreservation,
        ),
        (
            "preserves_envelope_provenance",
            ObservationUnknownWireCompatibilityAxis::EnvelopeProvenancePreservation,
        ),
    ] {
        let mut value = serde_json::to_value(&request).unwrap();
        value["capability"][field] = json!(false);
        let requested: ObservationUnknownWireContractRequest =
            serde_json::from_value(value).unwrap();
        assert_eq!(
            negotiate_observation_unknown_wire(&requested, &offer, &base).unwrap_err(),
            ObservationUnknownWireContractError::Incompatible {
                axis: expected_axis
            }
        );
    }

    let mut value = serde_json::to_value(&offer).unwrap();
    value["capability"]["unknown_wire_event_contract_version"] = json!(2);
    let offered: ObservationUnknownWireContractOffer = serde_json::from_value(value).unwrap();
    assert_eq!(
        negotiate_observation_unknown_wire(&request, &offered, &base).unwrap_err(),
        ObservationUnknownWireContractError::Incompatible {
            axis: ObservationUnknownWireCompatibilityAxis::EventContractVersion
        }
    );
}

#[test]
fn carrier_is_strict_bounded_portable_and_cannot_shadow_known_events() {
    let selected = selection();
    let base = carrier_wire();
    assert_eq!(
        ObservationUnknownWireEvent::from_wire_value(base.clone(), &selected)
            .unwrap()
            .wire_value(),
        base
    );

    for mutate in [
        |value: &mut JsonValue| value["future"] = json!(true),
        |value: &mut JsonValue| value["family"] = json!("source"),
        |value: &mut JsonValue| value["type_tag"] = json!("source_created"),
        |value: &mut JsonValue| value["type_tag"] = json!("Future Event"),
        |value: &mut JsonValue| value["envelope_provenance"]["observer_sequence"] = json!(0),
        |value: &mut JsonValue| value["envelope_provenance"]["scope_epoch"] = json!(0),
        |value: &mut JsonValue| value["envelope_provenance"]["event_id"] = json!("v1:AA"),
        |value: &mut JsonValue| value["envelope_provenance"]["source"]["generation"] = json!(0),
        |value: &mut JsonValue| value["envelope_provenance"]["phase"] = json!("future"),
        |value: &mut JsonValue| {
            value["envelope_provenance"]["observation_selection"]["event_contract_version"] =
                json!(2)
        },
        |value: &mut JsonValue| value["encoded_value"] = json!(1.5),
        |value: &mut JsonValue| value["encoded_value"] = json!({"__proto__": {"polluted": true}}),
    ] {
        let mut changed = base.clone();
        mutate(&mut changed);
        assert!(ObservationUnknownWireEvent::from_wire_value(changed, &selected).is_err());
    }

    let tiny = negotiate_observation_unknown_wire(&request(8), &offer(8), &observation_selection())
        .unwrap();
    assert!(ObservationUnknownWireEvent::from_wire_value(base.clone(), &tiny).is_err());

    let exact_encoded =
        negotiate_observation_unknown_wire(&request(16), &offer(16), &observation_selection())
            .unwrap();
    let mut escaped = base.clone();
    escaped["encoded_value"] = json!("\\".repeat(6));
    escaped["envelope_provenance"]["additional_envelope_provenance"] = json!({});
    assert!(
        ObservationUnknownWireEvent::from_wire_value(escaped, &exact_encoded).is_ok(),
        "six escaped bytes plus JSON quotes and an empty provenance object exactly fill 16 bytes"
    );
    let mut escaped = base.clone();
    escaped["encoded_value"] = json!("\\".repeat(7));
    escaped["envelope_provenance"]["additional_envelope_provenance"] = json!({});
    assert!(ObservationUnknownWireEvent::from_wire_value(escaped, &exact_encoded).is_err());

    let mut too_deep = json!(null);
    for _ in 0..=MAX_UNKNOWN_WIRE_DEPTH {
        too_deep = json!([too_deep]);
    }
    let mut changed = base.clone();
    changed["encoded_value"] = too_deep;
    assert!(ObservationUnknownWireEvent::from_wire_value(changed, &selected).is_err());

    let mut changed = base;
    changed["encoded_value"] = JsonValue::Array(
        (0..MAX_UNKNOWN_WIRE_NODES)
            .map(|_| JsonValue::Null)
            .collect(),
    );
    assert!(ObservationUnknownWireEvent::from_wire_value(changed, &selected).is_err());
}

#[test]
fn request_offer_and_selection_reject_unbounded_or_additive_authority() {
    for mut value in [
        serde_json::to_value(request(4_096)).unwrap(),
        serde_json::to_value(offer(4_096)).unwrap(),
    ] {
        value["future"] = json!(true);
        assert!(
            serde_json::from_value::<ObservationUnknownWireContractRequest>(value.clone()).is_err()
        );
        assert!(serde_json::from_value::<ObservationUnknownWireContractOffer>(value).is_err());
    }

    for bound in [0, MAX_UNKNOWN_WIRE_PRESERVED_BYTES + 1] {
        let mut value = serde_json::to_value(request(4_096)).unwrap();
        value["capability"]["max_preserved_bytes"] = json!(bound);
        assert!(serde_json::from_value::<ObservationUnknownWireContractRequest>(value).is_err());
    }
}
