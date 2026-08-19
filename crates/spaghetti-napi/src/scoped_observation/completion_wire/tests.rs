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
    ObservationContractRequest,
};

use super::super::*;
use super::*;

const FROZEN_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/rfc012d-scoped-completion-envelope-v1.json"
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

fn capability_context(
    compatibility: CompatibilityClass,
) -> Arc<ScopedCapabilitySnapshotConsumerContext> {
    let (offer, selection, _) = negotiation();
    let capabilities = ObservationCapabilities::from_negotiation(
        selection.clone(),
        &offer,
        compatibility,
        Some("fixture-support-v1"),
        &[("runtime.usage-v2", 1)],
    )
    .unwrap();
    Arc::new(
        ScopedCapabilitySnapshotConsumerContext::from_expected(
            &selection,
            &offer,
            compatibility,
            "fixture-support-v1",
            &capabilities,
        )
        .unwrap(),
    )
}

fn root() -> ScopedObservationRootIdentity {
    let adapter_id = AdapterId::new("fixture").unwrap();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"completion-source-instance").unwrap();
    let session_key = CanonicalEntityKey::derive(
        adapter_id.as_str(),
        &source_instance_key,
        "session",
        b"completion-session",
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
            b"completion-root-run",
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
            b"completion-session.jsonl",
        )
        .unwrap(),
    };
    let error = CoverageError {
        stream_key: Some(source.stream_key),
        object_key: Some(source.object_key),
        code: "source_io".to_owned(),
    };
    let declaration_digest =
        CoverageDeclarationDigest::derive(b"completion-scope-program").unwrap();
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
                    b"completion-cursor",
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
        CoverageMembershipRevision::derive(b"completion-membership").unwrap(),
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
        CoverageMembershipRevision::derive(b"completion-membership").unwrap(),
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
    let program_digest = crate::adapter::Sha256Digest::of(b"completion-scope-program");
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

fn family_manifest() -> Vec<ScopedReplacementFamilyManifest> {
    vec![ScopedReplacementFamilyManifest {
        fact_family: "runtime.usage-v2".to_owned(),
        contract_version: 1,
        replacement_representation:
            ScopedReplacementRepresentation::UsageLatestContributionPerResponse,
        completeness: CoverageSetCompleteness::Partial,
        entity_or_event_count: 0,
        semantic_digest: ScopedReplacementSemanticDigest([0x31; 32]),
    }]
}

struct FixturePair {
    bootstrap_context: super::ScopedCompletionEnvelopeConsumerContext,
    bootstrap_event: ScopedCompletionEnvelopeWire,
    resync_context: super::ScopedCompletionEnvelopeConsumerContext,
    resync_event: ScopedCompletionEnvelopeWire,
}

fn fixture_pair() -> FixturePair {
    let root = root();
    let (_, selection, capabilities) = negotiation();
    let capability_context = capability_context(CompatibilityClass::ExactSupported);
    let lifecycle = Arc::new(ScopedObservationAttachmentLifecycle::default());
    let mut drain = ScopedObservationConsumerDrain::new(
        ScopedObservationEnvelopeMapper::new(root.clone(), selection.clone()),
        Arc::clone(&capability_context),
        next_scoped_attachment_authority().unwrap(),
        Arc::clone(&lifecycle),
        ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 4,
        },
    )
    .unwrap();
    lifecycle
        .open_consumer_drain(&drain.delivery.event_completion)
        .unwrap();
    drain.lifecycle_registered = true;

    let (source_coverage, scope_coverage) = coverage_and_scope(&root);
    let artifact_availability = ScopedArtifactAvailabilitySnapshot::empty_fixture(root.session_key);
    let explicit_object_errors = canonical_explicit_errors(&source_coverage).unwrap();
    let watermark = ScopedObservationWatermarkCore {
        attachment_authority: Arc::clone(&drain.attachment_authority),
        root: root.clone(),
        scope_epoch: 1,
        offered_through_sequence: 0,
        source_coverage: source_coverage.clone(),
        observation_capabilities: capabilities.clone(),
        scope_coverage: scope_coverage.clone(),
        explicit_object_errors: explicit_object_errors.clone(),
        artifact_availability: artifact_availability.clone(),
        queue_state: drain.delivery.state(),
    };
    let bootstrap = drain
        .delivery
        .offer_bootstrap_barrier(&root, watermark, family_manifest(), true, 1_750_000_000)
        .unwrap();
    let yielded = drain.next().unwrap().unwrap();
    let bootstrap_context = yielded.completion_context().unwrap().clone();
    let bootstrap_event = ScopedCompletionEnvelopeWire::from_scoped_for_context(
        &yielded.envelope,
        &bootstrap_context,
    )
    .unwrap();
    drain
        .acknowledge_applied(yielded.application_receipt())
        .unwrap();

    drain
        .delivery
        .require_resync(
            &root,
            ScopedResyncReason::ExplicitConsumerRequest,
            1_750_000_001,
        )
        .unwrap();
    let required = drain.next().unwrap().unwrap();
    drain
        .acknowledge_applied(required.application_receipt())
        .unwrap();
    drain.delivery.begin_resync(&root, 1_750_000_002).unwrap();
    let started = drain.next().unwrap().unwrap();
    drain
        .acknowledge_applied(started.application_receipt())
        .unwrap();

    let correction_watermark = ScopedObservationWatermarkCore {
        attachment_authority: Arc::clone(&drain.attachment_authority),
        root: root.clone(),
        scope_epoch: 2,
        offered_through_sequence: 3,
        source_coverage: source_coverage.clone(),
        observation_capabilities: capabilities,
        scope_coverage: scope_coverage.clone(),
        explicit_object_errors,
        artifact_availability: artifact_availability.clone(),
        queue_state: drain.delivery.state(),
    };
    let components = ScopedCompletionSnapshotComponents {
        root: &root,
        root_present: true,
        family_manifest: &family_manifest(),
        observation_capabilities: &correction_watermark.observation_capabilities,
        scope_coverage: &scope_coverage,
        source_coverage: &source_coverage,
        explicit_object_errors: &correction_watermark.explicit_object_errors,
        artifact_availability: &artifact_availability,
    };
    let replacement_digest = replacement_snapshot_digest(components).unwrap();
    let resync = drain
        .delivery
        .offer_resync_barrier(
            &root,
            correction_watermark,
            family_manifest(),
            replacement_digest,
            true,
            1_750_000_003,
        )
        .unwrap();
    assert_eq!(
        bootstrap.replacement_snapshot_digest,
        resync.replacement_snapshot_digest
    );
    assert_eq!(bootstrap.snapshot_digest, resync.coverage_snapshot_digest);
    let yielded = drain.next().unwrap().unwrap();
    let resync_context = yielded.completion_context().unwrap().clone();
    let resync_event =
        ScopedCompletionEnvelopeWire::from_scoped_for_context(&yielded.envelope, &resync_context)
            .unwrap();
    FixturePair {
        bootstrap_context,
        bootstrap_event,
        resync_context,
        resync_event,
    }
}

fn fixture_value() -> Value {
    let fixture = fixture_pair();
    json!({
        "fixture_contract_version": 1,
        "bootstrap": {
            "context": fixture.bootstrap_context.wire(),
            "event": fixture.bootstrap_event,
        },
        "resync": {
            "context": fixture.resync_context.wire(),
            "event": fixture.resync_event,
        },
        "expected": {
            "ordered": true,
            "barrier_contract_version": 3,
            "replacement_digest_equal_at_equal_state": true,
            "coverage_digest_equal_at_equal_state": true,
            "rust_event_id_authority": "completion_snapshot_and_private_root",
            "portable_event_id_authority": "exact_rust_issued_context",
            "nested_contracts": [
                "capability_snapshot",
                "replacement_manifest",
                "scope_coverage",
                "artifact_availability",
                "rfc012a_source_coverage"
            ],
            "native_evidence": "engine_control_only",
            "source_access_authority": false,
            "task_artifact_discovery": false,
            "public_observer_transport": false,
            "native_payload_disclosure": "none"
        }
    })
}

fn parse(
    value: Value,
    context: &super::ScopedCompletionEnvelopeConsumerContext,
) -> Result<ScopedCompletionEnvelopeWire, ScopedCompletionEnvelopeContractError> {
    ScopedCompletionEnvelopeWire::from_wire_value_for_context(value, context)
}

#[test]
fn frozen_ordered_bootstrap_and_resync_completion_fixture_is_stable() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    if FROZEN_FIXTURE == "{}\n" {
        println!("{actual}");
    }
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture = fixture_pair();
    let frozen: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    for (name, context) in [
        ("bootstrap", &fixture.bootstrap_context),
        ("resync", &fixture.resync_context),
    ] {
        let parsed = parse(frozen[name]["event"].clone(), context).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), frozen[name]["event"]);
    }
    assert_eq!(
        frozen["bootstrap"]["event"]["event"]["barrier"]["replacement_snapshot_digest"],
        frozen["resync"]["event"]["event"]["barrier"]["replacement_snapshot_digest"]
    );
    assert_eq!(
        frozen["bootstrap"]["event"]["event"]["barrier"]["snapshot_digest"],
        frozen["resync"]["event"]["event"]["barrier"]["coverage_snapshot_digest"]
    );
}

#[test]
fn exact_order_root_source_phase_and_event_kind_are_context_bound() {
    let fixture = fixture_pair();
    let value = serde_json::to_value(&fixture.bootstrap_event).unwrap();
    for (field, replacement) in [
        ("observer_sequence", json!(2)),
        ("scope_epoch", json!(2)),
        ("event_id", json!(encode_opaque(&[0x55; 32]))),
        ("observed_at", json!(1_750_000_001_i64)),
        ("phase", json!("correction")),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        assert!(parse(changed, &fixture.bootstrap_context).is_err());
    }
    let mut root = value.clone();
    root["root"]["root_actor_run_key"] = root["root"]["session_key"].clone();
    assert!(parse(root, &fixture.bootstrap_context).is_err());

    let mut source = value.clone();
    source["source"]["generation"] = json!(2);
    assert!(parse(source, &fixture.bootstrap_context).is_err());

    let mut kind = value;
    kind["event"]["kind"] = json!("observer_resync_complete");
    assert!(parse(kind, &fixture.bootstrap_context).is_err());
}

#[test]
fn every_nested_completion_authority_and_digest_is_exact() {
    let fixture = fixture_pair();
    let value = serde_json::to_value(&fixture.bootstrap_event).unwrap();
    let mutations: &[&[&str]] = &[
        &["event", "barrier", "snapshot_digest"],
        &["event", "barrier", "replacement_snapshot_digest"],
        &[
            "event",
            "barrier",
            "replacement_manifest",
            "families",
            "0",
            "semantic_digest",
        ],
        &["event", "barrier", "capability_snapshot", "semantic_digest"],
        &[
            "event",
            "barrier",
            "source_coverage",
            "0",
            "membership_revision",
        ],
        &["event", "barrier", "scope_coverage", "scope_revision"],
        &["event", "barrier", "explicit_object_errors", "0", "code"],
        &[
            "event",
            "barrier",
            "artifact_availability",
            "semantic_digest",
        ],
    ];
    for path in mutations {
        let mut changed = value.clone();
        let mut cursor = &mut changed;
        for component in &path[..path.len() - 1] {
            cursor = if let Ok(index) = component.parse::<usize>() {
                &mut cursor[index]
            } else {
                &mut cursor[*component]
            };
        }
        let leaf = path[path.len() - 1];
        cursor[leaf] = if leaf == "code" {
            json!("source_unstable")
        } else {
            json!(encode_opaque(&[0x66; 32]))
        };
        assert!(
            parse(changed, &fixture.bootstrap_context).is_err(),
            "{path:?}"
        );
    }

    let mut queue = value;
    queue["event"]["barrier"]["queue_state"]["delivered_through_sequence"] = json!(1);
    assert!(parse(queue, &fixture.bootstrap_context).is_err());
}

#[test]
fn capability_support_context_and_resync_lineage_cannot_be_replayed() {
    let fixture = fixture_pair();
    let bootstrap = serde_json::to_value(&fixture.bootstrap_event).unwrap();
    let mut range_context = fixture.bootstrap_context.clone();
    range_context.capability_context = capability_context(CompatibilityClass::RangeSupported);
    assert!(parse(bootstrap, &range_context).is_err());

    let resync = serde_json::to_value(&fixture.resync_event).unwrap();
    let mut started = resync.clone();
    started["event"]["barrier"]["started_control_sequence"] = json!(2);
    assert!(parse(started, &fixture.resync_context).is_err());

    let mut replacement = resync.clone();
    replacement["event"]["barrier"]["replacement"] = json!("merge");
    assert!(parse(replacement, &fixture.resync_context).is_err());

    assert!(parse(resync, &fixture.bootstrap_context).is_err());

    let root = root();
    let (mut coverage, _) = coverage_and_scope(&root);
    coverage[0].scope.support_release_id = "different-support-v1".to_owned();
    assert!(!source_coverage_matches_authority(
        &root,
        &capability_context(CompatibilityClass::ExactSupported),
        &coverage,
    ));
}

#[test]
fn strict_shape_bounds_and_privacy_fail_closed() {
    let fixture = fixture_pair();
    let value = serde_json::to_value(&fixture.bootstrap_event).unwrap();
    let mut unknown = value.clone();
    unknown["future_meaning"] = json!(true);
    assert!(parse(unknown, &fixture.bootstrap_context).is_err());

    let mut nested_unknown = value.clone();
    nested_unknown["event"]["barrier"]["future_meaning"] = json!(true);
    assert!(parse(nested_unknown, &fixture.bootstrap_context).is_err());

    let mut zero = value.clone();
    zero["event"]["barrier"]["queue_state"]["scope_epoch"] = json!(0);
    assert!(parse(zero, &fixture.bootstrap_context).is_err());

    let encoded = serde_json::to_string(&fixture_value()).unwrap();
    for forbidden in [
        "locator_id\":\"",
        "native_payload\":",
        "source_record_id\":\"",
        "source_declaration_digest",
        "artifact_locator",
        "completion-source-instance",
        "completion-session.jsonl",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
    let debug = format!("{:?}", fixture.bootstrap_context);
    assert!(!debug.contains("fixture-support"));
    assert!(!debug.contains("session-observation"));
    assert!(!debug.contains("v1:"));
    assert!(!debug.contains("sha256:"));
}
