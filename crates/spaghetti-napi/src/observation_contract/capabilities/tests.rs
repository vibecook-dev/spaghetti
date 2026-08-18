use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::adapter::{ContractVersionOffer, ContractVersionRequest};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractRequest, OBSERVATION_BASE_MODEL_MAJOR,
    OBSERVATION_ENVELOPE_CONTRACT_VERSION, OBSERVATION_EVENT_CONTRACT_VERSION,
    OBSERVATION_LIFECYCLE_CONTRACT_VERSION, OBSERVATION_PROFILE_CONTRACT_VERSION,
};

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012d-observation-capabilities-v1.json"
));

fn negotiation() -> (
    ObservationContractRequest,
    ObservationContractOffer,
    ObservationContractSelection,
) {
    let request = ObservationContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: 1,
            model_major: OBSERVATION_BASE_MODEL_MAJOR,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: BTreeMap::from([("runtime.usage-v2".to_string(), vec![1])]),
            query_pack_versions: None,
            observation_contract_versions: Some(vec![OBSERVATION_PROFILE_CONTRACT_VERSION]),
        },
        vec![OBSERVATION_ENVELOPE_CONTRACT_VERSION],
        vec![OBSERVATION_EVENT_CONTRACT_VERSION],
        vec![OBSERVATION_LIFECYCLE_CONTRACT_VERSION],
    )
    .unwrap();
    let offer = ObservationContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: 1,
            model_major: OBSERVATION_BASE_MODEL_MAJOR,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: BTreeMap::from([
                ("runtime.actor-run".to_string(), vec![1]),
                ("runtime.usage-v2".to_string(), vec![1]),
            ]),
            query_pack_versions: Vec::new(),
            observation_contract_versions: vec![OBSERVATION_PROFILE_CONTRACT_VERSION],
        },
        vec![OBSERVATION_ENVELOPE_CONTRACT_VERSION],
        vec![OBSERVATION_EVENT_CONTRACT_VERSION],
        vec![OBSERVATION_LIFECYCLE_CONTRACT_VERSION],
    )
    .unwrap();
    let selection = negotiate_observation_contract(&request, &offer).unwrap();
    (request, offer, selection)
}

fn reports() -> (ObservationCapabilities, ObservationCapabilities) {
    let (_, offer, selection) = negotiation();
    let exact = ObservationCapabilities::from_negotiation(
        selection.clone(),
        &offer,
        CompatibilityClass::ExactSupported,
        Some("fixture-release-v1"),
        &[("runtime.usage-v2", 1)],
    )
    .unwrap();
    let range = ObservationCapabilities::from_negotiation(
        selection,
        &offer,
        CompatibilityClass::RangeSupported,
        Some("fixture-release-v1"),
        &[("runtime.usage-v2", 1)],
    )
    .unwrap();
    (exact, range)
}

#[test]
fn capabilities_distinguish_exact_range_and_unselected_families() {
    let (exact, range) = reports();
    assert_eq!(exact.fact_families.len(), 2);
    assert_eq!(exact.fact_families[0].fact_family, "runtime.actor-run");
    assert_eq!(
        exact.fact_families[0].status,
        ObservationCapabilityStatus::Unsupported
    );
    assert_eq!(exact.fact_families[0].selected_version, None);
    assert_eq!(exact.fact_families[1].fact_family, "runtime.usage-v2");
    assert_eq!(
        exact.fact_families[1].status,
        ObservationCapabilityStatus::Supported
    );
    assert_eq!(
        exact.fact_families[1].expected_completeness,
        ContractCompleteness::Complete
    );
    assert_eq!(
        range.fact_families[1].status,
        ObservationCapabilityStatus::Degraded
    );
    assert_eq!(
        range.fact_families[1].expected_completeness,
        ContractCompleteness::Partial
    );
    assert!(range.fact_families[1]
        .limitations
        .contains(&ObservationCapabilityLimitation::RangeSupportedNativeVersion));
}

#[test]
fn capabilities_reject_unauthorized_support_and_selection_offer_drift() {
    let (_, mut offer, selection) = negotiation();
    assert!(ObservationCapabilities::from_negotiation(
        selection.clone(),
        &offer,
        CompatibilityClass::RecognizedUnverified,
        Some("candidate-release"),
        &[("runtime.usage-v2", 1)]
    )
    .is_err());
    assert!(ObservationCapabilities::from_negotiation(
        selection.clone(),
        &offer,
        CompatibilityClass::ExactSupported,
        None,
        &[("runtime.usage-v2", 1)]
    )
    .is_err());
    let mut unimplemented = selection.clone();
    unimplemented
        .contract_versions
        .fact_family_versions
        .insert("runtime.actor-run".to_string(), 1);
    assert!(ObservationCapabilities::from_negotiation(
        unimplemented,
        &offer,
        CompatibilityClass::ExactSupported,
        Some("fixture-release-v1"),
        &[("runtime.usage-v2", 1)]
    )
    .is_err());
    offer
        .contract_versions
        .fact_family_versions
        .get_mut("runtime.usage-v2")
        .unwrap()
        .clear();
    assert!(ObservationCapabilities::from_negotiation(
        selection,
        &offer,
        CompatibilityClass::ExactSupported,
        Some("fixture-release-v1"),
        &[("runtime.usage-v2", 1)]
    )
    .is_err());
}

#[test]
fn capability_wire_is_selection_bound_strict_and_semantically_checked() {
    let (exact, _) = reports();
    let expected_selection = exact.selection.clone();
    let (_, offer, _) = negotiation();
    let parse = |value| {
        ObservationCapabilities::from_wire_value_for_context(
            value,
            &expected_selection,
            &offer,
            CompatibilityClass::ExactSupported,
            "fixture-release-v1",
        )
    };
    let wire = serde_json::to_value(&exact).unwrap();
    assert_eq!(parse(wire.clone()).unwrap(), exact);

    let mut foreign_selection = wire.clone();
    foreign_selection["selection"]["event_contract_version"] = json!(2);
    assert!(parse(foreign_selection).is_err());

    let mut unknown_top = wire.clone();
    unknown_top["extra"] = json!(true);
    assert!(parse(unknown_top).is_err());

    let mut unknown_family = wire.clone();
    unknown_family["fact_families"][0]["extra"] = json!(true);
    assert!(parse(unknown_family).is_err());

    let mut reordered = wire.clone();
    reordered["fact_families"].as_array_mut().unwrap().reverse();
    assert!(parse(reordered).is_err());

    let mut false_supported = wire.clone();
    false_supported["fact_families"][0]["status"] = json!("supported");
    assert!(parse(false_supported).is_err());

    let mut false_degraded = wire;
    false_degraded["fact_families"][1]["status"] = json!("degraded");
    assert!(parse(false_degraded).is_err());

    let exact_wire = serde_json::to_value(&exact).unwrap();
    assert!(ObservationCapabilities::from_wire_value_for_context(
        exact_wire.clone(),
        &expected_selection,
        &offer,
        CompatibilityClass::RangeSupported,
        "fixture-release-v1"
    )
    .is_err());
    assert!(ObservationCapabilities::from_wire_value_for_context(
        exact_wire.clone(),
        &expected_selection,
        &offer,
        CompatibilityClass::ExactSupported,
        "different-release"
    )
    .is_err());
    let mut narrowed_offer = offer.clone();
    narrowed_offer
        .contract_versions
        .fact_family_versions
        .remove("runtime.actor-run");
    assert!(ObservationCapabilities::from_wire_value_for_context(
        exact_wire,
        &expected_selection,
        &narrowed_offer,
        CompatibilityClass::ExactSupported,
        "fixture-release-v1"
    )
    .is_err());

    let mut wrong_selected_version = offer.clone();
    wrong_selected_version
        .contract_versions
        .fact_family_versions
        .insert("runtime.usage-v2".to_string(), vec![2]);
    assert!(ObservationCapabilities::from_wire_value_for_context(
        serde_json::to_value(&exact).unwrap(),
        &expected_selection,
        &wrong_selected_version,
        CompatibilityClass::ExactSupported,
        "fixture-release-v1"
    )
    .is_err());

    let mut wrong_event_offer = offer;
    wrong_event_offer.event_contract_versions = vec![2];
    assert!(ObservationCapabilities::from_wire_value_for_context(
        serde_json::to_value(&exact).unwrap(),
        &expected_selection,
        &wrong_event_offer,
        CompatibilityClass::ExactSupported,
        "fixture-release-v1"
    )
    .is_err());
}

#[test]
fn capability_wire_rejects_unbounded_noncanonical_and_duplicate_evidence() {
    let (exact, _) = reports();
    let selection = exact.selection.clone();
    let (_, offer, _) = negotiation();
    let parse = |value| {
        ObservationCapabilities::from_wire_value_for_context(
            value,
            &selection,
            &offer,
            CompatibilityClass::ExactSupported,
            "fixture-release-v1",
        )
    };
    let wire = serde_json::to_value(&exact).unwrap();

    let mut invalid_release = wire.clone();
    invalid_release["fact_families"][1]["evidence"]["support_release_id"] =
        json!(" fixture release ");
    assert!(parse(invalid_release).is_err());

    let mut duplicate_limit = wire.clone();
    duplicate_limit["fact_families"][1]["limitations"] = json!([
        "coverage_reported_separately",
        "coverage_reported_separately",
        "scope_bound"
    ]);
    assert!(parse(duplicate_limit).is_err());

    let mut zero_version = wire.clone();
    zero_version["fact_families"][0]["evidence"]["offered_versions"] = json!([0]);
    assert!(parse(zero_version).is_err());

    let mut oversized = wire;
    let template = oversized["fact_families"][0].clone();
    let families = oversized["fact_families"].as_array_mut().unwrap();
    for index in 0..MAX_CAPABILITY_FAMILIES {
        let mut entry = template.clone();
        entry["fact_family"] = json!(format!("runtime.unselected-{index:02}"));
        families.push(entry);
    }
    families.sort_by_key(|entry| entry["fact_family"].as_str().unwrap().to_string());
    assert!(parse(oversized).is_err());
}

#[test]
fn rust_capabilities_match_the_frozen_portable_fixture() {
    let (exact, range) = reports();
    let (_, offer, _) = negotiation();
    let actual = json!({
        "fixture_contract_version": 1,
        "contract_offer": offer,
        "exact": exact,
        "range": range,
    });
    let expected: serde_json::Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    assert_eq!(
        actual,
        expected,
        "repin with this Rust fixture:\n{}",
        serde_json::to_string_pretty(&actual).unwrap()
    );
}
