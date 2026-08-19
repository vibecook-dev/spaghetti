use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, CanonicalEntityKey, CanonicalSourceInstanceKey, CompatibilityClass,
    ContractVersionOffer, ContractVersionRequest, CoverageDeclarationDigest, CoverageDomain,
    CoverageError, CoverageMembershipRevision, CoveragePosition, CoveragePositionKind,
    CoverageProvenance, CoverageScope, CoverageSetCompleteness, CoverageStatus, ExternalEntityRef,
    SourceCoveragePoint, SourceCoverageSet,
};
use crate::observation_contract::{
    negotiate_observation_contract, ObservationCapabilities, ObservationContractOffer,
    ObservationContractRequest, ObservationContractSelection,
};

use super::super::*;
use super::*;

type WatermarkContext = super::ScopedObservationWatermarkConsumerContext;

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012d-scoped-watermark-v1.json"
));

fn negotiation() -> (
    ObservationContractOffer,
    ObservationContractSelection,
    ObservationCapabilities,
) {
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
    let selection = negotiate_observation_contract(&request, &offer).unwrap();
    let capabilities = ObservationCapabilities::from_negotiation(
        selection.clone(),
        &offer,
        CompatibilityClass::ExactSupported,
        Some("fixture-support-v1"),
        &[("runtime.usage-v2", 1)],
    )
    .unwrap();
    (offer, selection, capabilities)
}

fn capability_context() -> Arc<ScopedCapabilitySnapshotConsumerContext> {
    let (offer, selection, capabilities) = negotiation();
    Arc::new(
        ScopedCapabilitySnapshotConsumerContext::from_expected(
            &selection,
            &offer,
            CompatibilityClass::ExactSupported,
            "fixture-support-v1",
            &capabilities,
        )
        .unwrap(),
    )
}

fn root() -> ScopedObservationRootIdentity {
    let adapter_id = AdapterId::new("fixture").unwrap();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"watermark-source-instance").unwrap();
    let session_key = CanonicalEntityKey::derive(
        adapter_id.as_str(),
        &source_instance_key,
        "session",
        b"watermark-session",
    )
    .unwrap();
    ScopedObservationRootIdentity {
        adapter_id: adapter_id.clone(),
        source_instance_key,
        session_key,
        session_ref: ExternalEntityRef::new(session_key),
        root_actor_run_key: CanonicalEntityKey::derive(
            adapter_id.as_str(),
            &source_instance_key,
            "actor_run",
            b"watermark-root-run",
        )
        .unwrap(),
        native_session_claim: None,
    }
}

fn coverage_and_scope(
    root: &ScopedObservationRootIdentity,
) -> (Vec<SourceCoverageSet>, ScopedScopeCoverage) {
    let source = ScopedSourceObjectIdentity {
        adapter_id: root.adapter_id.clone(),
        source_instance_key: root.source_instance_key,
        stream_key: crate::adapter::CoverageStreamKey::derive(
            root.adapter_id.as_str(),
            b"transcript",
        )
        .unwrap(),
        object_key: crate::adapter::CoverageObjectKey::derive(
            "transcript",
            b"watermark-session.jsonl",
        )
        .unwrap(),
    };
    let error = CoverageError {
        stream_key: Some(source.stream_key),
        object_key: Some(source.object_key),
        code: "source_io".to_owned(),
    };
    let declaration_digest = CoverageDeclarationDigest::derive(b"watermark-scope-program").unwrap();
    let scope_value = CoverageScope {
        adapter_id: root.adapter_id.as_str().to_owned(),
        source_instance_key: root.source_instance_key,
        root_entity_key: Some(root.session_key),
        support_release_id: "fixture-support-v1".to_owned(),
        source_or_scope_declaration_digest: declaration_digest,
    };
    let point_for = |domain: CoverageDomain| {
        SourceCoveragePoint::new(
            domain,
            root.adapter_id.as_str(),
            root.source_instance_key,
            source.stream_key,
            source.object_key,
            3,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::AppendCursor,
                    b"watermark-cursor",
                    Some(7),
                )
                .unwrap(),
            ),
            CoverageStatus::CompleteThrough,
            CoverageProvenance::default(),
        )
        .unwrap()
    };
    let decode = SourceCoverageSet::new(
        CoverageDomain::Decode,
        scope_value.clone(),
        CoverageMembershipRevision::derive(b"watermark-membership").unwrap(),
        vec![point_for(CoverageDomain::Decode)],
        Vec::new(),
        vec![error.clone()],
        CoverageSetCompleteness::Partial,
    )
    .unwrap();
    let family_domain = CoverageDomain::FactFamily {
        family: "runtime.usage-v2".to_owned(),
        version: 1,
    };
    let family = SourceCoverageSet::new(
        family_domain.clone(),
        scope_value,
        CoverageMembershipRevision::derive(b"watermark-membership").unwrap(),
        vec![point_for(family_domain)],
        Vec::new(),
        vec![error],
        CoverageSetCompleteness::Partial,
    )
    .unwrap();
    let relations = vec![ScopedScopeRelationCoverage {
        relation_id: Arc::from("root-object"),
        scope_root: true,
        source,
        generation: 3,
        state: ScopedScopeRelationState::Present {
            status: CoverageStatus::CompleteThrough,
        },
        completeness: CoverageSetCompleteness::Partial,
    }];
    let program_digest = crate::adapter::Sha256Digest::of(b"watermark-scope-program");
    let scope = ScopedScopeCoverage {
        contract_version: SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION,
        program_id: "session-observation".to_owned(),
        scope_program_digest: program_digest,
        root_relation_id: Arc::from("root-object"),
        scope_revision: derive_scoped_scope_coverage_revision(
            "session-observation",
            program_digest,
            "root-object",
            root,
            &relations,
            CoverageSetCompleteness::Partial,
        ),
        relations,
        completeness: CoverageSetCompleteness::Partial,
    };
    let coverage = vec![decode, family];
    assert!(scope.validate_against(root, &coverage));
    (coverage, scope)
}

struct FixtureWatermark {
    authority: Arc<ScopedObservationAttachmentAuthority>,
    context: WatermarkContext,
    wire: ScopedObservationWatermarkWire,
    core: ScopedObservationWatermarkCore,
}

fn fixture_watermark(continuity: ScopedObservationContinuity) -> FixtureWatermark {
    let root = root();
    let (_, _, capabilities) = negotiation();
    let (source_coverage, scope_coverage) = coverage_and_scope(&root);
    let explicit_object_errors = canonical_explicit_errors(&source_coverage).unwrap();
    let authority = next_scoped_attachment_authority().unwrap();
    let (scope_epoch, offered_through_sequence, delivered_through_sequence) = match continuity {
        ScopedObservationContinuity::Bootstrap => (1, 0, 0),
        ScopedObservationContinuity::Valid => (2, 5, 4),
        _ => panic!("fixture accepts only publishable watermark continuity"),
    };
    let core = ScopedObservationWatermarkCore {
        attachment_authority: Arc::clone(&authority),
        root: root.clone(),
        scope_epoch,
        offered_through_sequence,
        source_coverage,
        observation_capabilities: capabilities,
        scope_coverage,
        explicit_object_errors,
        artifact_availability: ScopedArtifactAvailabilitySnapshot::empty_fixture(root.session_key),
        queue_state: ScopedObservationDeliveryState {
            scope_epoch,
            offered_through_sequence,
            delivered_through_sequence,
            continuity,
            queued_semantic_events: usize::from(continuity == ScopedObservationContinuity::Valid),
            queued_retained_native_bytes: 0,
            queued_source_control_items: 0,
        },
    };
    let context =
        WatermarkContext::from_scoped_watermark(&core, &authority, capability_context()).unwrap();
    let wire = ScopedObservationWatermarkWire::from_scoped_for_context(&core, &context).unwrap();
    FixtureWatermark {
        authority,
        context,
        wire,
        core,
    }
}

fn fixture_value() -> Value {
    let live = fixture_watermark(ScopedObservationContinuity::Valid);
    json!({
        "fixture_contract_version": 1,
        "context": live.context.wire(),
        "watermark": live.wire,
        "expected": {
            "request_generation_is_flow_control_only": true,
            "capability_and_support_context_bound": true,
            "source_and_scope_coverage_exact": true,
            "artifact_availability_state_bound": true,
            "queue_continuity": ["bootstrap", "valid"],
            "source_access_authority": false,
            "public_observer_transport": false,
            "native_locator_or_payload": false,
        }
    })
}

fn parse(
    value: Value,
    context: &WatermarkContext,
) -> Result<ScopedObservationWatermarkWire, ScopedObservationWatermarkContractError> {
    ScopedObservationWatermarkWire::from_wire_value_for_context(value, context)
}

#[test]
fn frozen_bootstrap_and_live_watermark_fixture_is_stable() {
    let actual = format!("{}\n", serde_json::to_string(&fixture_value()).unwrap());
    if FROZEN_FIXTURE == "{}\n" {
        println!("{actual}");
    }
    assert_eq!(actual, FROZEN_FIXTURE);

    let frozen: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    let live = fixture_watermark(ScopedObservationContinuity::Valid);
    let parsed = parse(frozen["watermark"].clone(), &live.context).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), frozen["watermark"]);

    let bootstrap = fixture_watermark(ScopedObservationContinuity::Bootstrap);
    let bootstrap_value = serde_json::to_value(&bootstrap.wire).unwrap();
    let parsed = parse(bootstrap_value.clone(), &bootstrap.context).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), bootstrap_value);
}

#[test]
fn exact_attachment_root_selection_and_support_are_context_bound() {
    let fixture = fixture_watermark(ScopedObservationContinuity::Valid);
    let value = serde_json::to_value(&fixture.wire).unwrap();

    let foreign = next_scoped_attachment_authority().unwrap();
    assert!(
        WatermarkContext::from_scoped_watermark(&fixture.core, &foreign, capability_context(),)
            .is_err()
    );
    assert!(!Arc::ptr_eq(&fixture.authority, &foreign));

    for path in [
        &["root", "root_actor_run_key"][..],
        &["contract_selection", "event_contract_version"][..],
        &["capability_snapshot", "semantic_digest"][..],
        &["source_coverage", "0", "membership_revision"][..],
        &["scope_coverage", "scope_revision"][..],
        &["artifact_availability", "semantic_digest"][..],
    ] {
        let mut changed = value.clone();
        let mut cursor = &mut changed;
        for component in &path[..path.len() - 1] {
            cursor = if let Ok(index) = component.parse::<usize>() {
                &mut cursor[index]
            } else {
                &mut cursor[*component]
            };
        }
        cursor[path[path.len() - 1]] = if path[path.len() - 1].ends_with("version") {
            json!(2)
        } else {
            json!("v1:ZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmY")
        };
        assert!(parse(changed, &fixture.context).is_err(), "{path:?}");
    }
}

#[test]
fn selected_family_coverage_and_canonical_errors_are_mandatory() {
    let fixture = fixture_watermark(ScopedObservationContinuity::Bootstrap);
    let mut missing_family = fixture.core.clone();
    missing_family.source_coverage.pop();
    missing_family.explicit_object_errors =
        canonical_explicit_errors(&missing_family.source_coverage).unwrap();
    assert!(WatermarkContext::from_scoped_watermark(
        &missing_family,
        &fixture.authority,
        capability_context(),
    )
    .is_err());

    let mut duplicate_family = fixture.core.clone();
    duplicate_family
        .source_coverage
        .push(duplicate_family.source_coverage[1].clone());
    assert!(WatermarkContext::from_scoped_watermark(
        &duplicate_family,
        &fixture.authority,
        capability_context(),
    )
    .is_err());

    let mut support_drift = fixture.core.clone();
    support_drift.source_coverage[0].scope.support_release_id = "fixture-support-v2".to_owned();
    assert!(WatermarkContext::from_scoped_watermark(
        &support_drift,
        &fixture.authority,
        capability_context(),
    )
    .is_err());

    let mut error_drift = fixture.core.clone();
    error_drift.explicit_object_errors.clear();
    assert!(WatermarkContext::from_scoped_watermark(
        &error_drift,
        &fixture.authority,
        capability_context(),
    )
    .is_err());
}

#[test]
fn queue_boundary_and_continuity_fail_closed() {
    let fixture = fixture_watermark(ScopedObservationContinuity::Valid);
    let value = serde_json::to_value(&fixture.wire).unwrap();
    for (field, replacement) in [
        ("scope_epoch", json!(0)),
        ("offered_through_sequence", json!(4)),
        ("delivered_through_sequence", json!(5)),
        ("continuity", json!("resyncing")),
        ("queued_semantic_events", json!(2)),
    ] {
        let mut changed = value.clone();
        changed["queue_state"][field] = replacement;
        assert!(parse(changed, &fixture.context).is_err(), "{field}");
    }
    for continuity in [
        ScopedObservationContinuity::ResyncRequired,
        ScopedObservationContinuity::Resyncing,
        ScopedObservationContinuity::Failed,
    ] {
        let mut core = fixture.core.clone();
        core.queue_state.continuity = continuity;
        assert!(WatermarkContext::from_scoped_watermark(
            &core,
            &fixture.authority,
            capability_context(),
        )
        .is_err());
    }
}

#[test]
fn strict_shape_bounds_and_privacy_fail_closed() {
    let fixture = fixture_watermark(ScopedObservationContinuity::Valid);
    let value = serde_json::to_value(&fixture.wire).unwrap();
    let mut unknown = value.clone();
    unknown["future_meaning"] = json!(true);
    assert!(parse(unknown, &fixture.context).is_err());

    let mut oversized = value;
    oversized["source_coverage"] = json!(vec![Value::Null; MAX_SOURCE_COVERAGE_SETS + 1]);
    assert!(parse(oversized, &fixture.context).is_err());

    let encoded = serde_json::to_string(&fixture_value()).unwrap();
    for forbidden in [
        "locator_id\":\"",
        "native_payload\":",
        "watermark-source-instance",
        "watermark-session.jsonl",
        "/Users/",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
    let debug = format!("{:?}", fixture.context);
    assert!(!debug.contains("fixture-support"));
    assert!(!debug.contains("session-observation"));
    assert!(!debug.contains("v1:"));
    assert!(!debug.contains("sha256:"));
}

#[test]
fn completed_poll_retains_exact_context_without_flow_control_identity() {
    let fixture = fixture_watermark(ScopedObservationContinuity::Valid);
    let watermark = Arc::new(fixture.core.clone());
    let completed = super::ScopedObservationCompletedPoll::from_resolved(
        Arc::clone(&watermark),
        fixture.context.clone(),
    )
    .unwrap();
    assert!(Arc::ptr_eq(completed.watermark(), &watermark));
    assert_eq!(
        completed.watermark_wire_value().unwrap(),
        serde_json::to_value(&fixture.wire).unwrap()
    );
    assert_eq!(
        completed.context_wire_value().unwrap(),
        serde_json::to_value(fixture.context.wire()).unwrap()
    );

    let encoded = format!(
        "{}{}",
        completed.watermark_wire_value().unwrap(),
        completed.context_wire_value().unwrap()
    );
    assert!(!encoded.contains("request_generation"));
    let debug = format!("{completed:?}");
    assert!(debug.contains("scope_epoch: 2"));
    assert!(debug.contains("offered_through_sequence: 5"));
    assert!(!debug.contains("fixture-support"));
    assert!(!debug.contains("session-observation"));
    assert!(!debug.contains("v1:"));
    assert!(!debug.contains("sha256:"));

    let foreign = fixture_watermark(ScopedObservationContinuity::Valid);
    assert!(super::ScopedObservationCompletedPoll::from_resolved(
        Arc::clone(&watermark),
        foreign.context,
    )
    .is_err());
}
