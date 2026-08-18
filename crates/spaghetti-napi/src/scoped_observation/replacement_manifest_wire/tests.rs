use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{
    CanonicalSourceInstanceKey, ContractVersionOffer, ContractVersionRequest,
    CoverageDeclarationDigest, CoverageDomain, CoverageMembershipRevision, CoverageScope,
    CoverageSetCompleteness, SourceCoverageSet,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationContractOffer, ObservationContractRequest,
};

use super::super::*;
use super::*;

const ACTOR_AFFILIATION_FAMILY: &str = "runtime.actor-affiliation";
const ACTOR_RUN_FAMILY: &str = "runtime.actor-run";
const USAGE_V2_FAMILY: &str = "runtime.usage-v2";
const FROZEN_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-replacement-manifest-v1.json");

fn contract_selection() -> ObservationContractSelection {
    let families = BTreeMap::from([
        (ACTOR_AFFILIATION_FAMILY.to_owned(), vec![1]),
        (ACTOR_RUN_FAMILY.to_owned(), vec![1]),
        (USAGE_V2_FAMILY.to_owned(), vec![1]),
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

fn coverage_set(
    domain: CoverageDomain,
    completeness: CoverageSetCompleteness,
    discriminator: &str,
) -> SourceCoverageSet {
    SourceCoverageSet::new(
        domain,
        CoverageScope {
            adapter_id: "fixture".to_owned(),
            source_instance_key: CanonicalSourceInstanceKey::derive(
                1,
                b"fixture-scoped-replacement-source",
            )
            .unwrap(),
            root_entity_key: None,
            support_release_id: "fixture-support-v1".to_owned(),
            source_or_scope_declaration_digest: CoverageDeclarationDigest::derive(
                b"fixture-scoped-replacement-declaration",
            )
            .unwrap(),
        },
        CoverageMembershipRevision::derive(discriminator.as_bytes()).unwrap(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        completeness,
    )
    .unwrap()
}

fn source_coverage() -> Vec<SourceCoverageSet> {
    vec![
        coverage_set(
            CoverageDomain::Decode,
            CoverageSetCompleteness::Complete,
            "fixture-replacement-decode-membership",
        ),
        coverage_set(
            CoverageDomain::FactFamily {
                family: ACTOR_AFFILIATION_FAMILY.to_owned(),
                version: 1,
            },
            CoverageSetCompleteness::Complete,
            "fixture-replacement-affiliation-membership",
        ),
        coverage_set(
            CoverageDomain::FactFamily {
                family: ACTOR_RUN_FAMILY.to_owned(),
                version: 1,
            },
            CoverageSetCompleteness::Complete,
            "fixture-replacement-actor-membership-a",
        ),
        coverage_set(
            CoverageDomain::FactFamily {
                family: ACTOR_RUN_FAMILY.to_owned(),
                version: 1,
            },
            CoverageSetCompleteness::Partial,
            "fixture-replacement-actor-membership-b",
        ),
        coverage_set(
            CoverageDomain::FactFamily {
                family: USAGE_V2_FAMILY.to_owned(),
                version: 1,
            },
            CoverageSetCompleteness::Unavailable,
            "fixture-replacement-usage-membership",
        ),
    ]
}

fn expected_families() -> Vec<ScopedReplacementFamilyManifest> {
    vec![
        ScopedReplacementFamilyManifest {
            fact_family: ACTOR_AFFILIATION_FAMILY.to_owned(),
            contract_version: 1,
            replacement_representation: ScopedReplacementRepresentation::RevisionedEntityCurrent,
            completeness: CoverageSetCompleteness::Complete,
            entity_or_event_count: 2,
            semantic_digest: ScopedReplacementSemanticDigest([0x11; 32]),
        },
        ScopedReplacementFamilyManifest {
            fact_family: ACTOR_RUN_FAMILY.to_owned(),
            contract_version: 1,
            replacement_representation: ScopedReplacementRepresentation::RevisionedEntityCurrent,
            completeness: CoverageSetCompleteness::Partial,
            entity_or_event_count: 3,
            semantic_digest: ScopedReplacementSemanticDigest([0x22; 32]),
        },
        ScopedReplacementFamilyManifest {
            fact_family: USAGE_V2_FAMILY.to_owned(),
            contract_version: 1,
            replacement_representation:
                ScopedReplacementRepresentation::UsageLatestContributionPerResponse,
            completeness: CoverageSetCompleteness::Unavailable,
            entity_or_event_count: 4,
            semantic_digest: ScopedReplacementSemanticDigest([0x33; 32]),
        },
    ]
}

fn context() -> ScopedReplacementManifestConsumerContext {
    ScopedReplacementManifestConsumerContext::from_expected(
        &contract_selection(),
        &source_coverage(),
        &expected_families(),
    )
    .unwrap()
}

fn fixture_value() -> Value {
    let context = context();
    json!({
        "fixture_contract_version": 1,
        "context": context.wire(),
        "manifest": ScopedReplacementManifestWire::from_context(&context).unwrap(),
        "expected": {
            "selected_fact_families": [
                ACTOR_AFFILIATION_FAMILY,
                ACTOR_RUN_FAMILY,
                USAGE_V2_FAMILY,
            ],
            "completeness_by_family": {
                ACTOR_AFFILIATION_FAMILY: "complete",
                ACTOR_RUN_FAMILY: "partial",
                USAGE_V2_FAMILY: "unavailable",
            },
            "phase_independent": true,
            "bootstrap_or_resync_barrier": false,
            "source_access_authority": false,
            "portable_observer_transport": false,
            "native_payload_disclosure": "none",
        },
    })
}

fn parse(
    value: Value,
    context: &ScopedReplacementManifestConsumerContext,
) -> Result<ScopedReplacementManifestWire, ScopedReplacementManifestContractError> {
    ScopedReplacementManifestWire::from_wire_value_for_context(value, context)
}

#[test]
fn frozen_replacement_manifest_is_stable_contextual_and_non_authorizing() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    if FROZEN_FIXTURE == "{}\n" {
        println!("{actual}");
    }
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    let context = context();
    let parsed = parse(fixture["manifest"].clone(), &context).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), fixture["manifest"]);
    assert!(fixture["manifest"].get("phase").is_none());
    assert!(fixture["manifest"].get("root").is_none());
    assert!(fixture["manifest"].get("source_access").is_none());
    assert!(fixture["manifest"].get("native_payload").is_none());
}

#[test]
fn exact_selection_expected_manifest_and_merged_coverage_are_required() {
    let context = context();
    let value =
        serde_json::to_value(ScopedReplacementManifestWire::from_context(&context).unwrap())
            .unwrap();

    let mut selection = value.clone();
    selection["contract_selection"]["contract_versions"]["fact_family_versions"]
        [ACTOR_RUN_FAMILY] = json!(2);
    assert!(parse(selection, &context).is_err());

    let mut completeness = value.clone();
    completeness["families"][1]["completeness"] = json!("complete");
    assert!(parse(completeness, &context).is_err());

    let mut digest = value.clone();
    digest["families"][1]["semantic_digest"] = json!(encode_opaque(&[0x44; 32]));
    assert_eq!(
        parse(digest, &context),
        Err(ScopedReplacementManifestContractError::ContextMismatch)
    );

    let mut count = value;
    count["families"][1]["entity_or_event_count"] = json!(4);
    assert_eq!(
        parse(count, &context),
        Err(ScopedReplacementManifestContractError::ContextMismatch)
    );

    let mut reversed_coverage = source_coverage();
    reversed_coverage.reverse();
    let reversed_context = ScopedReplacementManifestConsumerContext::from_expected(
        &contract_selection(),
        &reversed_coverage,
        &expected_families(),
    )
    .unwrap();
    assert_eq!(
        ScopedReplacementManifestWire::from_context(&reversed_context).unwrap(),
        ScopedReplacementManifestWire::from_context(&context).unwrap()
    );
}

#[test]
fn missing_foreign_and_projection_coverage_fail_before_wire_consumption() {
    let selection = contract_selection();
    let expected = expected_families();

    let mut missing = source_coverage();
    missing.retain(|coverage| {
        !matches!(
            &coverage.coverage_domain,
            CoverageDomain::FactFamily { family, .. } if family == USAGE_V2_FAMILY
        )
    });
    assert!(ScopedReplacementManifestConsumerContext::from_expected(
        &selection, &missing, &expected
    )
    .is_err());

    let mut foreign = source_coverage();
    foreign.push(coverage_set(
        CoverageDomain::FactFamily {
            family: "runtime.foreign".to_owned(),
            version: 1,
        },
        CoverageSetCompleteness::Complete,
        "fixture-foreign-family-membership",
    ));
    assert!(ScopedReplacementManifestConsumerContext::from_expected(
        &selection, &foreign, &expected
    )
    .is_err());

    let mut projection = source_coverage();
    projection.push(coverage_set(
        CoverageDomain::ProjectionPack {
            pack: "library.catalog".to_owned(),
            version: 1,
        },
        CoverageSetCompleteness::Complete,
        "fixture-projection-membership",
    ));
    assert!(ScopedReplacementManifestConsumerContext::from_expected(
        &selection,
        &projection,
        &expected,
    )
    .is_err());

    let mut wrong_expected = expected;
    wrong_expected[1].completeness = CoverageSetCompleteness::Complete;
    assert!(ScopedReplacementManifestConsumerContext::from_expected(
        &selection,
        &source_coverage(),
        &wrong_expected,
    )
    .is_err());
}

#[test]
fn strict_shapes_order_counts_and_digests_fail_closed() {
    let context = context();
    let value =
        serde_json::to_value(ScopedReplacementManifestWire::from_context(&context).unwrap())
            .unwrap();

    let mut top_unknown = value.clone();
    top_unknown["future_meaning"] = json!(true);
    assert!(parse(top_unknown, &context).is_err());

    let mut family_unknown = value.clone();
    family_unknown["families"][0]["future_meaning"] = json!(true);
    assert!(parse(family_unknown, &context).is_err());

    let mut reordered = value.clone();
    reordered["families"].as_array_mut().unwrap().swap(0, 1);
    assert!(parse(reordered, &context).is_err());

    let mut duplicate = value.clone();
    duplicate["families"][1] = duplicate["families"][0].clone();
    assert!(parse(duplicate, &context).is_err());

    let mut zero_digest = value.clone();
    zero_digest["families"][0]["semantic_digest"] = json!(encode_opaque(&[0; 32]));
    assert!(parse(zero_digest, &context).is_err());

    let mut short_digest = value.clone();
    short_digest["families"][0]["semantic_digest"] = json!(encode_opaque(&[1; 31]));
    assert!(parse(short_digest, &context).is_err());

    let mut unsafe_count = value;
    unsafe_count["families"][0]["entity_or_event_count"] = json!(JS_SAFE_INTEGER_MAX + 1);
    assert!(parse(unsafe_count, &context).is_err());

    let mut invalid_expected = expected_families();
    invalid_expected[0].semantic_digest = ScopedReplacementSemanticDigest([0; 32]);
    assert!(ScopedReplacementManifestConsumerContext::from_expected(
        &contract_selection(),
        &source_coverage(),
        &invalid_expected,
    )
    .is_err());
}

#[test]
fn context_bounds_and_debug_withhold_manifest_and_coverage_details() {
    let selection = contract_selection();
    let expected = expected_families();
    let one = coverage_set(
        CoverageDomain::FactFamily {
            family: ACTOR_AFFILIATION_FAMILY.to_owned(),
            version: 1,
        },
        CoverageSetCompleteness::Complete,
        "fixture-context-cap-membership",
    );
    let oversized = vec![one; MAX_CONTEXT_COVERAGE_SETS + 1];
    assert!(ScopedReplacementManifestConsumerContext::from_expected(
        &selection, &oversized, &expected
    )
    .is_err());

    let debug = format!("{:?}", context());
    assert!(!debug.contains("runtime.actor"));
    assert!(!debug.contains("v1:"));
    assert!(!debug.contains("fixture-support"));
    assert!(!debug.contains("ERERER"));
}
