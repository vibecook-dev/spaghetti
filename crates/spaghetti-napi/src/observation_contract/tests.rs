use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::json;

use super::*;

const USAGE_FAMILY: &str = "runtime.usage-v2";

fn contract_request() -> ContractVersionRequest {
    ContractVersionRequest {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: OBSERVATION_BASE_MODEL_MAJOR,
        external_entity_reference_version: EXTERNAL_ENTITY_REFERENCE_VERSION,
        semantic_revision_reference_version: SEMANTIC_REFERENCE_CONTRACT_VERSION,
        coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
        fact_family_versions: BTreeMap::from([(USAGE_FAMILY.to_owned(), vec![1])]),
        query_pack_versions: None,
        observation_contract_versions: Some(vec![OBSERVATION_PROFILE_CONTRACT_VERSION]),
    }
}

fn contract_offer() -> ContractVersionOffer {
    ContractVersionOffer {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: OBSERVATION_BASE_MODEL_MAJOR,
        external_entity_reference_versions: vec![EXTERNAL_ENTITY_REFERENCE_VERSION],
        semantic_revision_reference_versions: vec![SEMANTIC_REFERENCE_CONTRACT_VERSION],
        coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
        fact_family_versions: BTreeMap::from([(USAGE_FAMILY.to_owned(), vec![1])]),
        query_pack_versions: vec![],
        observation_contract_versions: vec![OBSERVATION_PROFILE_CONTRACT_VERSION],
    }
}

fn request() -> ObservationContractRequest {
    ObservationContractRequest::new(contract_request(), vec![1], vec![1], vec![1]).unwrap()
}

fn offer() -> ObservationContractOffer {
    ObservationContractOffer::new(contract_offer(), vec![1], vec![1], vec![1]).unwrap()
}

fn axis(error: ObservationNegotiationError) -> ObservationCompatibilityAxis {
    let ObservationNegotiationError::IncompatibleObservationContract { axis } = error else {
        panic!("expected typed incompatibility, got {error}")
    };
    axis
}

#[test]
fn negotiation_composes_rfc012a_and_selects_the_exact_v1_profile() {
    let mut requested = request();
    requested.contract_versions.observation_contract_versions = Some(vec![2, 1]);
    requested.envelope_contract_versions = vec![2, 1];
    requested.event_contract_versions = vec![2, 1];
    requested.lifecycle_contract_versions = vec![2, 1];
    let selection = negotiate_observation_contract(&requested, &offer()).unwrap();
    assert_eq!(selection.contract_versions.query_pack_version, None);
    assert_eq!(
        selection.contract_versions.observation_contract_version,
        Some(1)
    );
    assert_eq!(
        selection.contract_versions.fact_family_versions[USAGE_FAMILY],
        1
    );
    assert_eq!(selection.envelope_contract_version, 1);
    assert_eq!(selection.event_contract_version, 1);
    assert_eq!(selection.lifecycle_contract_version, 1);
}

#[test]
fn every_incompatible_axis_is_typed_before_native_access_can_begin() {
    let base_request = request();
    let base_offer = offer();

    let mut requested = base_request.clone();
    let mut offered = base_offer.clone();
    requested.contract_versions.model_major = 2;
    offered.contract_versions.model_major = 2;
    assert_eq!(
        axis(negotiate_observation_contract(&requested, &offered).unwrap_err()),
        ObservationCompatibilityAxis::BaseModelMajor
    );

    let mut requested = base_request.clone();
    let mut offered = base_offer.clone();
    requested
        .contract_versions
        .external_entity_reference_version = 2;
    offered.contract_versions.external_entity_reference_versions = vec![2];
    assert_eq!(
        axis(negotiate_observation_contract(&requested, &offered).unwrap_err()),
        ObservationCompatibilityAxis::ExternalEntityReferenceVersion
    );

    let mut requested = base_request.clone();
    let mut offered = base_offer.clone();
    requested
        .contract_versions
        .semantic_revision_reference_version = 2;
    offered
        .contract_versions
        .semantic_revision_reference_versions = vec![2];
    assert_eq!(
        axis(negotiate_observation_contract(&requested, &offered).unwrap_err()),
        ObservationCompatibilityAxis::SemanticRevisionReferenceVersion
    );

    let mut requested = base_request.clone();
    let mut offered = base_offer.clone();
    requested.contract_versions.coverage_contract_versions = vec![2];
    offered.contract_versions.coverage_contract_versions = vec![2];
    assert_eq!(
        axis(negotiate_observation_contract(&requested, &offered).unwrap_err()),
        ObservationCompatibilityAxis::CoverageContractVersion
    );

    let mut requested = base_request.clone();
    requested
        .contract_versions
        .fact_family_versions
        .insert(USAGE_FAMILY.to_owned(), vec![2]);
    assert_eq!(
        axis(negotiate_observation_contract(&requested, &base_offer).unwrap_err()),
        ObservationCompatibilityAxis::FactFamilyVersion
    );

    let mut requested = base_request.clone();
    let mut offered = base_offer.clone();
    requested.contract_versions.observation_contract_versions = Some(vec![2]);
    offered.contract_versions.observation_contract_versions = vec![2];
    assert_eq!(
        axis(negotiate_observation_contract(&requested, &offered).unwrap_err()),
        ObservationCompatibilityAxis::ObservationProfileVersion
    );

    for (expected, mutate) in [
        (ObservationCompatibilityAxis::EnvelopeContractVersion, 0_u8),
        (ObservationCompatibilityAxis::EventContractVersion, 1_u8),
        (ObservationCompatibilityAxis::LifecycleContractVersion, 2_u8),
    ] {
        let mut requested = base_request.clone();
        let mut offered = base_offer.clone();
        match mutate {
            0 => {
                requested.envelope_contract_versions = vec![2];
                offered.envelope_contract_versions = vec![2];
            }
            1 => {
                requested.event_contract_versions = vec![2];
                offered.event_contract_versions = vec![2];
            }
            _ => {
                requested.lifecycle_contract_versions = vec![2];
                offered.lifecycle_contract_versions = vec![2];
            }
        }
        assert_eq!(
            axis(negotiate_observation_contract(&requested, &offered).unwrap_err()),
            expected
        );
    }
}

#[test]
fn wire_shapes_are_strict_bounded_and_cannot_smuggle_query_authority() {
    let request_value = serde_json::to_value(request()).unwrap();
    let parsed: ObservationContractRequest = serde_json::from_value(request_value.clone()).unwrap();
    assert_eq!(parsed, request());

    let mut unknown = request_value.clone();
    unknown["future_request_meaning"] = json!(true);
    assert!(serde_json::from_value::<ObservationContractRequest>(unknown).is_err());

    let mut query = request_value.clone();
    query["contract_versions"]["query_pack_versions"] = json!([1]);
    assert!(serde_json::from_value::<ObservationContractRequest>(query).is_err());

    let mut query_offer = serde_json::to_value(offer()).unwrap();
    query_offer["contract_versions"]["query_pack_versions"] = json!([1]);
    assert!(serde_json::from_value::<ObservationContractOffer>(query_offer).is_err());

    let mut invalid_family = request_value.clone();
    invalid_family["contract_versions"]["fact_family_versions"] = json!({ "Invalid Family": [1] });
    assert!(serde_json::from_value::<ObservationContractRequest>(invalid_family).is_err());

    let mut duplicate = request_value.clone();
    duplicate["event_contract_versions"] = json!([1, 1]);
    assert!(serde_json::from_value::<ObservationContractRequest>(duplicate).is_err());

    let mut oversized = request_value;
    oversized["lifecycle_contract_versions"] =
        json!((1_u32..=MAX_VERSION_PREFERENCES as u32 + 1).collect::<Vec<_>>());
    assert!(serde_json::from_value::<ObservationContractRequest>(oversized).is_err());

    let selection = negotiate_observation_contract(&request(), &offer()).unwrap();
    let selection_value = serde_json::to_value(&selection).unwrap();
    let parsed = ObservationContractSelection::from_wire_value_for_negotiation(
        selection_value.clone(),
        &request(),
        &offer(),
    )
    .unwrap();
    assert_eq!(parsed, selection);
    let mut forged = selection_value;
    forged["event_contract_version"] = json!(2);
    assert!(ObservationContractSelection::from_wire_value_for_negotiation(
        forged,
        &request(),
        &offer(),
    )
    .is_err());

    let mut retargeted = serde_json::to_value(selection).unwrap();
    retargeted["contract_versions"]["fact_family_versions"] = json!({ "runtime.other-v1": 1 });
    assert!(
        ObservationContractSelection::from_wire_value_for_negotiation(
            retargeted,
            &request(),
            &offer(),
        )
        .is_err()
    );

    let mut downgraded =
        serde_json::to_value(negotiate_observation_contract(&request(), &offer()).unwrap())
            .unwrap();
    downgraded["contract_versions"]["fact_family_versions"][USAGE_FAMILY] = json!(2);
    assert!(
        ObservationContractSelection::from_wire_value_for_negotiation(
            downgraded,
            &request(),
            &offer(),
        )
        .is_err()
    );
}

#[test]
fn selected_event_contract_matches_the_internal_scoped_event_identity_contract() {
    assert_eq!(
        OBSERVATION_EVENT_CONTRACT_VERSION,
        crate::scoped_observation::SCOPED_OBSERVATION_EVENT_CONTRACT_VERSION
    );
}

#[derive(Debug, Serialize)]
struct ObservationNegotiationFixture {
    fixture_contract_version: u32,
    contract_request: ObservationContractRequest,
    contract_offer: ObservationContractOffer,
    contract_selection: ObservationContractSelection,
    expected: FixtureExpected,
}

#[derive(Debug, Serialize)]
struct FixtureExpected {
    incompatible_error: &'static str,
    selected_fact_family: &'static str,
    selected_fact_family_version: u32,
    query_pack_selected: bool,
    typed_unknown_event_preservation: &'static str,
}

fn frozen_fixture() -> ObservationNegotiationFixture {
    ObservationNegotiationFixture {
        fixture_contract_version: 1,
        contract_request: request(),
        contract_offer: offer(),
        contract_selection: negotiate_observation_contract(&request(), &offer()).unwrap(),
        expected: FixtureExpected {
            incompatible_error: "IncompatibleObservationContract",
            selected_fact_family: USAGE_FAMILY,
            selected_fact_family_version: 1,
            query_pack_selected: false,
            typed_unknown_event_preservation: "not_yet_negotiated",
        },
    }
}

#[test]
fn rust_observation_negotiation_matches_the_frozen_portable_fixture() {
    let actual = serde_json::to_value(frozen_fixture()).unwrap();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../fixtures/contracts/rfc012d-observation-negotiation-v1.json"
    ))
    .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&actual).unwrap());
    assert_eq!(actual, expected);
}
