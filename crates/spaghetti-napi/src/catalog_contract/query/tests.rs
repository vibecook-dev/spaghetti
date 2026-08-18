use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::super::{
    contract_digest, CatalogCoveragePlanId, CatalogCoverageScope, CatalogQueryKind, CatalogSortKey,
};
use super::*;
use crate::adapter::{CanonicalEntityKey, CanonicalSourceInstanceKey};

fn contract_request() -> CatalogQueryContractRequest {
    CatalogQueryContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: CATALOG_BASE_MODEL_MAJOR,
            external_entity_reference_version: EXTERNAL_ENTITY_REFERENCE_VERSION,
            semantic_revision_reference_version: SEMANTIC_REFERENCE_CONTRACT_VERSION,
            coverage_contract_versions: vec![2, SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions: BTreeMap::new(),
            query_pack_versions: Some(vec![2, CATALOG_QUERY_PACK_CONTRACT_VERSION]),
            observation_contract_versions: None,
        },
        CatalogTypedUnknownCapability::preserving(4_096).unwrap(),
    )
    .unwrap()
}

fn contract_offer() -> CatalogQueryContractOffer {
    CatalogQueryContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: CATALOG_BASE_MODEL_MAJOR,
            external_entity_reference_versions: vec![EXTERNAL_ENTITY_REFERENCE_VERSION],
            semantic_revision_reference_versions: vec![SEMANTIC_REFERENCE_CONTRACT_VERSION],
            coverage_contract_versions: vec![SOURCE_COVERAGE_CONTRACT_VERSION],
            fact_family_versions: BTreeMap::new(),
            query_pack_versions: vec![CATALOG_QUERY_PACK_CONTRACT_VERSION],
            observation_contract_versions: Vec::new(),
        },
        CatalogTypedUnknownCapability::preserving(8_192).unwrap(),
    )
    .unwrap()
}

fn selection() -> CatalogQueryContractSelection {
    negotiate_catalog_query_contract(&contract_request(), &contract_offer()).unwrap()
}

fn continuation(selection: CatalogQueryContractSelection) -> CatalogContinuationRequest {
    let coverage_plan_id = CatalogCoveragePlanId::from_digest(contract_digest(
        b"catalog-query-fixture-plan",
        &[b"portable-v1"],
    ));
    let snapshot_id =
        CatalogSnapshotId::new(CATALOG_QUERY_PACK_CONTRACT_VERSION, coverage_plan_id, 7, 42)
            .unwrap();
    let query_fingerprint = CatalogQueryFingerprint::derive(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        CatalogQueryKind::Sessions,
        CatalogCoverageScope::Library,
        1,
        br#"{"availability":"any","project":null}"#,
    )
    .unwrap();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"catalog-query-fixture-source").unwrap();
    let last_entity_key = CanonicalEntityKey::derive(
        "fixture-agent",
        &source_instance_key,
        "session",
        b"last-session",
    )
    .unwrap();
    let cursor = CatalogCursor::new(
        snapshot_id,
        query_fingerprint,
        1,
        CatalogSortKey::new(b"2026-08-17T12:00:00Z".to_vec()).unwrap(),
        last_entity_key,
    )
    .unwrap();
    CatalogContinuationRequest::new(selection, snapshot_id, query_fingerprint, 1, cursor, 50)
        .unwrap()
}

fn assert_incompatible(
    result: Result<CatalogQueryContractSelection, CatalogQueryNegotiationError>,
    expected_axis: CatalogQueryCompatibilityAxis,
) {
    assert_eq!(
        result.unwrap_err(),
        CatalogQueryNegotiationError::IncompatibleCatalogContract {
            axis: expected_axis
        }
    );
}

#[test]
fn negotiation_selects_supported_preferences_and_rejects_each_incompatible_axis() {
    let selected = selection();
    assert_eq!(
        selected.contract_versions.coverage_contract_version,
        SOURCE_COVERAGE_CONTRACT_VERSION
    );
    assert_eq!(
        selected.contract_versions.query_pack_version,
        Some(CATALOG_QUERY_PACK_CONTRACT_VERSION)
    );
    assert_eq!(selected.typed_unknown.max_payload_bytes, 4_096);

    let mut request = contract_request();
    let mut offer = contract_offer();
    request.contract_versions.model_major = 2;
    offer.contract_versions.model_major = 2;
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &offer),
        CatalogQueryCompatibilityAxis::BaseModelMajor,
    );

    let mut request = contract_request();
    let mut offer = contract_offer();
    request.contract_versions.external_entity_reference_version = 2;
    offer.contract_versions.external_entity_reference_versions = vec![2];
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &offer),
        CatalogQueryCompatibilityAxis::ExternalEntityReferenceVersion,
    );

    let mut request = contract_request();
    let mut offer = contract_offer();
    request
        .contract_versions
        .semantic_revision_reference_version = 2;
    offer.contract_versions.semantic_revision_reference_versions = vec![2];
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &offer),
        CatalogQueryCompatibilityAxis::SemanticRevisionReferenceVersion,
    );

    let mut request = contract_request();
    let mut offer = contract_offer();
    request.contract_versions.coverage_contract_versions = vec![2];
    offer.contract_versions.coverage_contract_versions = vec![2];
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &offer),
        CatalogQueryCompatibilityAxis::CoverageContractVersion,
    );

    let mut request = contract_request();
    let mut offer = contract_offer();
    request.contract_versions.query_pack_versions = Some(vec![2]);
    offer.contract_versions.query_pack_versions = vec![2];
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &offer),
        CatalogQueryCompatibilityAxis::QueryPackVersion,
    );

    let mut request = contract_request();
    let mut offer = contract_offer();
    request
        .contract_versions
        .fact_family_versions
        .insert("catalog.session".to_owned(), vec![1]);
    offer
        .contract_versions
        .fact_family_versions
        .insert("catalog.session".to_owned(), vec![2]);
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &offer),
        CatalogQueryCompatibilityAxis::FactFamilyVersion,
    );

    let mut request = contract_request();
    let mut offer = contract_offer();
    request.contract_versions.observation_contract_versions = Some(vec![1]);
    offer.contract_versions.observation_contract_versions = vec![2];
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &offer),
        CatalogQueryCompatibilityAxis::ObservationContractVersion,
    );

    let mut request = contract_request();
    request.typed_unknown.preserves_unknown_variants = false;
    assert_incompatible(
        negotiate_catalog_query_contract(&request, &contract_offer()),
        CatalogQueryCompatibilityAxis::TypedUnknownPreservation,
    );
}

#[test]
fn malformed_query_negotiation_versions_and_preferences_are_rejected() {
    let mut wrong_wrapper = serde_json::to_value(contract_request()).unwrap();
    wrong_wrapper["catalog_query_contract_version"] = json!(2);
    assert!(serde_json::from_value::<CatalogQueryContractRequest>(wrong_wrapper).is_err());

    let mut missing_pack = serde_json::to_value(contract_request()).unwrap();
    missing_pack["contract_versions"]["query_pack_versions"] = Value::Null;
    assert!(serde_json::from_value::<CatalogQueryContractRequest>(missing_pack).is_err());

    let mut duplicate_pack = serde_json::to_value(contract_request()).unwrap();
    duplicate_pack["contract_versions"]["query_pack_versions"] = json!([1, 1]);
    assert!(serde_json::from_value::<CatalogQueryContractRequest>(duplicate_pack).is_err());

    let mut forged_selection = serde_json::to_value(selection()).unwrap();
    forged_selection["contract_versions"]["query_pack_version"] = json!(2);
    assert!(serde_json::from_value::<CatalogQueryContractSelection>(forged_selection).is_err());
}

#[test]
fn additive_fields_and_future_variants_round_trip_as_bounded_typed_unknowns() {
    let selected_contract = selection();
    let additive_wire = json!({
        "catalog_query_response_contract_version": CATALOG_QUERY_RESPONSE_CONTRACT_VERSION,
        "contract_selection": selected_contract,
        "kind": "selected",
        "future_server_hint": {
            "mode": "bounded",
            "retry_after": 2
        }
    });
    let parsed =
        CatalogQueryContractResponse::from_wire_value(additive_wire.clone(), &selected_contract)
            .unwrap();
    let CatalogQueryContractResponse::Selected {
        additive_fields, ..
    } = &parsed
    else {
        panic!("selected response must remain selected");
    };
    assert_eq!(
        additive_fields.get("future_server_hint"),
        Some(&json!({"mode": "bounded", "retry_after": 2}))
    );
    assert_eq!(parsed.to_wire_value().unwrap(), additive_wire);

    let unknown_wire = json!({
        "catalog_query_response_contract_version": CATALOG_QUERY_RESPONSE_CONTRACT_VERSION,
        "contract_selection": selection(),
        "kind": "future_catalog_capability",
        "capability": {
            "name": "server_rank_hint",
            "enabled": true
        }
    });
    let parsed =
        CatalogQueryContractResponse::from_wire_value(unknown_wire.clone(), &selected_contract)
            .unwrap();
    let CatalogQueryContractResponse::TypedUnknown {
        variant, payload, ..
    } = &parsed
    else {
        panic!("future response variant must become typed unknown");
    };
    assert_eq!(variant, "future_catalog_capability");
    assert_eq!(
        payload.get("capability"),
        Some(&json!({"name": "server_rank_hint", "enabled": true}))
    );
    assert_eq!(parsed.to_wire_value().unwrap(), unknown_wire);

    assert!(CatalogQueryContractResponse::selected(
        selection(),
        BTreeMap::from([("kind".to_owned(), json!("cannot_replace_discriminant"))]),
    )
    .is_err());

    let mut drifted_selection = additive_wire;
    drifted_selection["contract_selection"]["typed_unknown"]["max_payload_bytes"] = json!(8_192);
    assert!(
        CatalogQueryContractResponse::from_wire_value(drifted_selection, &selected_contract,)
            .is_err()
    );
}

#[test]
fn typed_unknowns_reject_wrong_major_oversize_depth_and_nonportable_numbers() {
    let expected_selection = selection();
    let mut wrong_major = json!({
        "catalog_query_response_contract_version": 2,
        "contract_selection": expected_selection,
        "kind": "selected"
    });
    assert!(CatalogQueryContractResponse::from_wire_value(
        wrong_major.clone(),
        &expected_selection
    )
    .is_err());

    wrong_major["catalog_query_response_contract_version"] = json!(1);
    wrong_major["oversized"] = json!("x".repeat(4_096));
    assert!(
        CatalogQueryContractResponse::from_wire_value(wrong_major, &expected_selection).is_err()
    );

    let mut nested = json!(null);
    for _ in 0..=MAX_TYPED_UNKNOWN_DEPTH {
        nested = json!([nested]);
    }
    let too_deep = json!({
        "catalog_query_response_contract_version": 1,
        "contract_selection": expected_selection,
        "kind": "selected",
        "future_nested": nested
    });
    assert!(CatalogQueryContractResponse::from_wire_value(too_deep, &expected_selection).is_err());

    let float = json!({
        "catalog_query_response_contract_version": 1,
        "contract_selection": expected_selection,
        "kind": "selected",
        "future_number": 1.5
    });
    assert!(CatalogQueryContractResponse::from_wire_value(float, &expected_selection).is_err());
}

#[test]
fn continuation_is_bound_to_selected_pack_snapshot_fingerprint_and_sort() {
    let expected_selection = selection();
    let continuation = continuation(expected_selection.clone());
    let wire = serde_json::to_value(&continuation).unwrap();
    assert_eq!(
        CatalogContinuationRequest::from_wire_value(wire.clone(), &expected_selection).unwrap(),
        continuation
    );

    let mut wrong_snapshot = wire.clone();
    wrong_snapshot["cursor"]["snapshot_id"]["complete_commit"] = json!(43);
    assert!(
        CatalogContinuationRequest::from_wire_value(wrong_snapshot, &expected_selection).is_err()
    );

    let other_fingerprint = CatalogQueryFingerprint::derive(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        CatalogQueryKind::Projects,
        CatalogCoverageScope::Library,
        1,
        b"{}",
    )
    .unwrap();
    let mut wrong_query = wire.clone();
    wrong_query["cursor"]["query_fingerprint"] = serde_json::to_value(other_fingerprint).unwrap();
    assert!(CatalogContinuationRequest::from_wire_value(wrong_query, &expected_selection).is_err());

    let mut wrong_sort = wire.clone();
    wrong_sort["cursor"]["sort_spec_version"] = json!(2);
    assert!(CatalogContinuationRequest::from_wire_value(wrong_sort, &expected_selection).is_err());

    let mut wrong_pack = wire.clone();
    wrong_pack["snapshot_id"]["pack_contract_version"] = json!(2);
    wrong_pack["cursor"]["snapshot_id"]["pack_contract_version"] = json!(2);
    assert!(CatalogContinuationRequest::from_wire_value(wrong_pack, &expected_selection).is_err());

    let mut nonportable_commit = wire.clone();
    nonportable_commit["snapshot_id"]["complete_commit"] = json!(MAX_JAVASCRIPT_SAFE_INTEGER + 1);
    nonportable_commit["cursor"]["snapshot_id"]["complete_commit"] =
        json!(MAX_JAVASCRIPT_SAFE_INTEGER + 1);
    assert!(
        CatalogContinuationRequest::from_wire_value(nonportable_commit, &expected_selection)
            .is_err()
    );

    let mut nonportable_epoch = wire.clone();
    nonportable_epoch["snapshot_id"]["readiness_epoch"] = json!(MAX_JAVASCRIPT_SAFE_INTEGER + 1);
    nonportable_epoch["cursor"]["snapshot_id"]["readiness_epoch"] =
        json!(MAX_JAVASCRIPT_SAFE_INTEGER + 1);
    assert!(
        CatalogContinuationRequest::from_wire_value(nonportable_epoch, &expected_selection)
            .is_err()
    );

    let mut drifted_selection = wire.clone();
    drifted_selection["contract_selection"]["typed_unknown"]["max_payload_bytes"] = json!(8_192);
    assert!(
        CatalogContinuationRequest::from_wire_value(drifted_selection, &expected_selection)
            .is_err()
    );

    let mut zero_page = wire;
    zero_page["page_size"] = json!(0);
    assert!(CatalogContinuationRequest::from_wire_value(zero_page, &expected_selection).is_err());
}

fn frozen_fixture() -> Value {
    let request = contract_request();
    let offer = contract_offer();
    let selected_contract = negotiate_catalog_query_contract(&request, &offer).unwrap();
    let selected_response =
        CatalogQueryContractResponse::selected(selected_contract.clone(), BTreeMap::new())
            .unwrap()
            .to_wire_value()
            .unwrap();
    let additive_response = json!({
        "catalog_query_response_contract_version": CATALOG_QUERY_RESPONSE_CONTRACT_VERSION,
        "contract_selection": selected_contract,
        "kind": "selected",
        "future_server_hint": {
            "mode": "bounded",
            "retry_after": 2
        }
    });
    CatalogQueryContractResponse::from_wire_value(additive_response.clone(), &selected_contract)
        .unwrap();
    let unknown_variant_response = json!({
        "catalog_query_response_contract_version": CATALOG_QUERY_RESPONSE_CONTRACT_VERSION,
        "contract_selection": selection(),
        "kind": "future_catalog_capability",
        "capability": {
            "enabled": true,
            "name": "server_rank_hint"
        }
    });
    CatalogQueryContractResponse::from_wire_value(
        unknown_variant_response.clone(),
        &selected_contract,
    )
    .unwrap();
    let continuation_request = continuation(selection());

    json!({
        "fixture_contract_version": 1,
        "contract_request": request,
        "contract_offer": offer,
        "selected_response": selected_response,
        "selected_response_with_additive_field": additive_response,
        "unknown_response_variant": unknown_variant_response,
        "continuation_request": continuation_request,
        "expected": {
            "selected_query_pack_version": CATALOG_QUERY_PACK_CONTRACT_VERSION,
            "selected_unknown_max_payload_bytes": 4_096,
            "additive_field": "future_server_hint",
            "unknown_variant": "future_catalog_capability",
            "cursor_binding_valid": true,
            "incompatible_error": "IncompatibleCatalogContract"
        }
    })
}

#[test]
fn frozen_catalog_query_contract_matches_portable_fixture() {
    let actual = frozen_fixture();
    let expected = serde_json::from_str::<Value>(include_str!(
        "../../../fixtures/contracts/rfc012b-catalog-query-v1.json"
    ))
    .unwrap();
    if actual != expected {
        eprintln!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
    assert_eq!(actual, expected);
}
