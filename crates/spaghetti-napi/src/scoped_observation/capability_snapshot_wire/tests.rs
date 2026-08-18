use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{CompatibilityClass, ContractVersionOffer, ContractVersionRequest};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationCapabilities, ObservationContractOffer,
    ObservationContractRequest, ObservationContractSelection, OBSERVATION_BASE_MODEL_MAJOR,
    OBSERVATION_ENVELOPE_CONTRACT_VERSION, OBSERVATION_EVENT_CONTRACT_VERSION,
    OBSERVATION_LIFECYCLE_CONTRACT_VERSION, OBSERVATION_PROFILE_CONTRACT_VERSION,
};

use super::*;

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012d-scoped-capability-snapshot-v1.json"
));

fn negotiation() -> (ObservationContractOffer, ObservationContractSelection) {
    let request = ObservationContractRequest::new(
        ContractVersionRequest {
            selection_contract_version: 1,
            model_major: OBSERVATION_BASE_MODEL_MAJOR,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_versions: vec![1],
            fact_family_versions: BTreeMap::from([("runtime.usage-v2".to_owned(), vec![1])]),
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
                ("runtime.actor-run".to_owned(), vec![1]),
                ("runtime.usage-v2".to_owned(), vec![1]),
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
    (offer, selection)
}

fn capabilities(
    compatibility: CompatibilityClass,
) -> (
    ObservationContractOffer,
    ObservationContractSelection,
    ObservationCapabilities,
) {
    let (offer, selection) = negotiation();
    let capabilities = ObservationCapabilities::from_negotiation(
        selection.clone(),
        &offer,
        compatibility,
        Some("fixture-release-v1"),
        &[("runtime.usage-v2", 1)],
    )
    .unwrap();
    (offer, selection, capabilities)
}

fn context(compatibility: CompatibilityClass) -> ScopedCapabilitySnapshotConsumerContext {
    let (offer, selection, capabilities) = capabilities(compatibility);
    ScopedCapabilitySnapshotConsumerContext::from_expected(
        &selection,
        &offer,
        compatibility,
        "fixture-release-v1",
        &capabilities,
    )
    .unwrap()
}

fn fixture_value() -> Value {
    let exact = context(CompatibilityClass::ExactSupported);
    let range = context(CompatibilityClass::RangeSupported);
    json!({
        "fixture_contract_version": 1,
        "exact": {
            "context": exact.wire(),
            "snapshot": ScopedCapabilitySnapshotWire::from_context(&exact).unwrap(),
        },
        "range": {
            "context": range.wire(),
            "snapshot": ScopedCapabilitySnapshotWire::from_context(&range).unwrap(),
        },
        "expected": {
            "selected_family": "runtime.usage-v2",
            "unselected_family": "runtime.actor-run",
            "phase_independent": true,
            "coverage_or_readiness_claim": false,
            "bootstrap_or_resync_barrier": false,
            "artifact_availability_state": false,
            "source_access_authority": false,
            "portable_observer_transport": false,
            "native_payload_disclosure": "none",
            "semantic_digest_algorithm": "blake3-256",
        },
    })
}

fn parse(
    value: Value,
    context: &ScopedCapabilitySnapshotConsumerContext,
) -> Result<ScopedCapabilitySnapshotWire, ScopedCapabilitySnapshotContractError> {
    ScopedCapabilitySnapshotWire::from_wire_value_for_context(value, context)
}

#[test]
fn frozen_capability_snapshots_are_stable_contextual_and_non_authorizing() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    if FROZEN_FIXTURE == "{}\n" {
        println!("{actual}");
    }
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    for (name, compatibility) in [
        ("exact", CompatibilityClass::ExactSupported),
        ("range", CompatibilityClass::RangeSupported),
    ] {
        let context = context(compatibility);
        let parsed = parse(fixture[name]["snapshot"].clone(), &context).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            fixture[name]["snapshot"]
        );
    }
    assert_ne!(
        fixture["exact"]["snapshot"]["semantic_digest"],
        fixture["range"]["snapshot"]["semantic_digest"]
    );
}

#[test]
fn support_negotiation_and_digest_expectation_are_exact_context() {
    let exact = context(CompatibilityClass::ExactSupported);
    let range = context(CompatibilityClass::RangeSupported);
    let value =
        serde_json::to_value(ScopedCapabilitySnapshotWire::from_context(&exact).unwrap()).unwrap();

    assert!(parse(value.clone(), &range).is_err());

    let mut selection = value.clone();
    selection["observation_capabilities"]["selection"]["event_contract_version"] = json!(2);
    assert!(parse(selection, &exact).is_err());

    let mut digest = value.clone();
    digest["semantic_digest"] = json!(encode_opaque(&[0x55; DIGEST_BYTES]));
    assert_eq!(
        parse(digest, &exact),
        Err(ScopedCapabilitySnapshotContractError::ContextMismatch)
    );

    let (offer, selection, capabilities) = capabilities(CompatibilityClass::ExactSupported);
    assert!(ScopedCapabilitySnapshotConsumerContext::from_expected(
        &selection,
        &offer,
        CompatibilityClass::RangeSupported,
        "fixture-release-v1",
        &capabilities,
    )
    .is_err());
    assert!(ScopedCapabilitySnapshotConsumerContext::from_expected(
        &selection,
        &offer,
        CompatibilityClass::ExactSupported,
        "different-release",
        &capabilities,
    )
    .is_err());
}

#[test]
fn strict_shape_versions_family_bounds_and_digests_fail_closed() {
    let context = context(CompatibilityClass::ExactSupported);
    let value = serde_json::to_value(ScopedCapabilitySnapshotWire::from_context(&context).unwrap())
        .unwrap();

    let mut top_unknown = value.clone();
    top_unknown["future_meaning"] = json!(true);
    assert!(parse(top_unknown, &context).is_err());

    let mut nested_unknown = value.clone();
    nested_unknown["observation_capabilities"]["future_meaning"] = json!(true);
    assert!(parse(nested_unknown, &context).is_err());

    let mut family_unknown = value.clone();
    family_unknown["observation_capabilities"]["fact_families"][0]["future_meaning"] = json!(true);
    assert!(parse(family_unknown, &context).is_err());

    let mut version = value.clone();
    version["capability_digest_contract_version"] = json!(2);
    assert!(parse(version, &context).is_err());

    let mut zero = value.clone();
    zero["semantic_digest"] = json!(encode_opaque(&[0; DIGEST_BYTES]));
    assert!(parse(zero, &context).is_err());

    let mut short = value.clone();
    short["semantic_digest"] = json!(encode_opaque(&[1; DIGEST_BYTES - 1]));
    assert!(parse(short, &context).is_err());

    let mut oversized = value;
    let family = oversized["observation_capabilities"]["fact_families"][0].clone();
    oversized["observation_capabilities"]["fact_families"] =
        json!(vec![family; MAX_CAPABILITY_FAMILIES + 1]);
    assert!(parse(oversized, &context).is_err());
}

#[test]
fn canonical_capability_content_alone_determines_the_phase_independent_digest() {
    let (_, _, exact_capabilities) = capabilities(CompatibilityClass::ExactSupported);
    let (_, _, range_capabilities) = capabilities(CompatibilityClass::RangeSupported);
    let first = ScopedCapabilitySnapshotWire::from_capabilities(&exact_capabilities).unwrap();
    let replay = ScopedCapabilitySnapshotWire::from_capabilities(&exact_capabilities).unwrap();
    let range = ScopedCapabilitySnapshotWire::from_capabilities(&range_capabilities).unwrap();
    assert_eq!(first, replay);
    assert_ne!(first.semantic_digest, range.semantic_digest);

    let value = serde_json::to_value(first).unwrap();
    for field in [
        "phase",
        "root",
        "source_coverage",
        "current_readiness",
        "artifact_availability",
        "barrier_sequence",
        "source_access",
        "native_payload",
    ] {
        assert!(value.get(field).is_none());
    }
}

#[test]
fn context_debug_withholds_support_family_and_digest_details() {
    let debug = format!("{:?}", context(CompatibilityClass::ExactSupported));
    assert!(debug.contains("selected_family_count"));
    assert!(debug.contains("offered_family_count"));
    assert!(!debug.contains("fixture-release"));
    assert!(!debug.contains("runtime."));
    assert!(!debug.contains("v1:"));
}
