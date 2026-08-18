use std::sync::Arc;

use serde_json::{json, Value};

use crate::adapter::{
    AdapterId, CanonicalEntityKey, ContractCompleteness, CoverageAbsence,
    CoverageDeclarationDigest, CoverageDomain, CoverageMembershipRevision, CoveragePosition,
    CoveragePositionKind, CoverageProvenance, CoverageScope, ExternalEntityRef,
    FactSemanticContext, NativeIdentity, NativeIdentityClaim, QualifiedValue,
    QualifiedValueQuality, SourceCoveragePoint,
};

use super::super::*;
use super::*;

const FROZEN_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-scope-coverage-v1.json");

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

fn source(root: &ScopedObservationRootIdentity, name: &[u8]) -> ScopedSourceObjectIdentity {
    ScopedSourceObjectIdentity {
        adapter_id: root.adapter_id.clone(),
        source_instance_key: root.source_instance_key,
        stream_key: CoverageStreamKey::derive(root.adapter_id.as_str(), b"transcript").unwrap(),
        object_key: CoverageObjectKey::derive("transcript", name).unwrap(),
    }
}

fn fixture_values() -> (
    ScopedObservationRootIdentity,
    Vec<SourceCoverageSet>,
    ScopedScopeCoverage,
) {
    let root = root_identity();
    let root_source = source(&root, b"root-session.jsonl");
    let summary_source = source(&root, b"root-session.summary.json");
    let relations = vec![
        ScopedScopeRelationCoverage {
            relation_id: Arc::from("root-object"),
            scope_root: true,
            source: root_source.clone(),
            generation: 3,
            state: ScopedScopeRelationState::Present {
                status: CoverageStatus::CompleteThrough,
            },
            completeness: CoverageSetCompleteness::Complete,
        },
        ScopedScopeRelationCoverage {
            relation_id: Arc::from("summary-sidecar"),
            scope_root: false,
            source: summary_source.clone(),
            generation: 2,
            state: ScopedScopeRelationState::Absent {
                kind: CoverageAbsenceKind::Absent,
            },
            completeness: CoverageSetCompleteness::Complete,
        },
    ];
    let program_digest = Sha256Digest::of(b"fixture-scope-program");
    let scope = ScopedScopeCoverage {
        contract_version: SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION,
        program_id: "session-observation".to_owned(),
        scope_program_digest: program_digest,
        root_relation_id: Arc::from("root-object"),
        scope_revision: derive_scoped_scope_coverage_revision(
            "session-observation",
            program_digest,
            "root-object",
            &root,
            &relations,
            CoverageSetCompleteness::Complete,
        ),
        relations,
        completeness: CoverageSetCompleteness::Complete,
    };
    let decode = SourceCoverageSet::new(
        CoverageDomain::Decode,
        CoverageScope {
            adapter_id: root.adapter_id.as_str().to_owned(),
            source_instance_key: root.source_instance_key,
            root_entity_key: Some(root.session_key),
            support_release_id: "fixture-support-v1".to_owned(),
            source_or_scope_declaration_digest: CoverageDeclarationDigest::derive(
                b"fixture-scope-program",
            )
            .unwrap(),
        },
        CoverageMembershipRevision::derive(b"fixture-scope-membership").unwrap(),
        vec![SourceCoveragePoint::new(
            CoverageDomain::Decode,
            root.adapter_id.as_str(),
            root.source_instance_key,
            root_source.stream_key,
            root_source.object_key,
            3,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::AppendCursor,
                    b"fixture-cursor",
                    Some(7),
                )
                .unwrap(),
            ),
            CoverageStatus::CompleteThrough,
            CoverageProvenance::default(),
        )
        .unwrap()],
        vec![CoverageAbsence {
            stream_key: summary_source.stream_key,
            object_key: summary_source.object_key,
            generation: 2,
            kind: CoverageAbsenceKind::Absent,
        }],
        Vec::new(),
        CoverageSetCompleteness::Complete,
    )
    .unwrap();
    assert!(scope.validate_against(&root, std::slice::from_ref(&decode)));
    (root, vec![decode], scope)
}

fn fixture_value() -> Value {
    let (root, source_coverage, scope) = fixture_values();
    let context =
        ScopedScopeCoverageConsumerContext::from_expected(&scope, &root, &source_coverage).unwrap();
    let wire = ScopedScopeCoverageWire::from_expected(&scope, &context).unwrap();
    json!({ "context": context.wire(), "scope_coverage": wire })
}

fn parse_mutated(
    mutator: impl FnOnce(&mut Value),
) -> Result<ScopedScopeCoverageWire, ScopedScopeCoverageContractError> {
    let (root, source_coverage, scope) = fixture_values();
    let context =
        ScopedScopeCoverageConsumerContext::from_expected(&scope, &root, &source_coverage).unwrap();
    let mut value =
        serde_json::to_value(ScopedScopeCoverageWire::from_expected(&scope, &context).unwrap())
            .unwrap();
    mutator(&mut value);
    ScopedScopeCoverageWire::from_wire_value_for_context(value, &context)
}

#[test]
fn frozen_rust_scope_coverage_fixture_is_stable() {
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&fixture_value()).unwrap()
    );
    assert_eq!(actual, FROZEN_FIXTURE);

    let fixture: Value = serde_json::from_str(FROZEN_FIXTURE).unwrap();
    let (root, source_coverage, scope) = fixture_values();
    let context =
        ScopedScopeCoverageConsumerContext::from_expected(&scope, &root, &source_coverage).unwrap();
    let parsed = ScopedScopeCoverageWire::from_wire_value_for_context(
        fixture["scope_coverage"].clone(),
        &context,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap(),
        fixture["scope_coverage"]
    );
}

#[test]
fn program_root_and_declared_relation_set_are_caller_held() {
    for field in ["program_id", "scope_program_digest", "root_relation_id"] {
        assert!(parse_mutated(|value| {
            value[field] = if field == "scope_program_digest" {
                Value::String(Sha256Digest::of(b"foreign-program").to_string())
            } else {
                Value::String("foreign".to_owned())
            };
        })
        .is_err());
    }
    assert!(parse_mutated(|value| {
        value["relations"].as_array_mut().unwrap().remove(1);
    })
    .is_err());
    assert!(parse_mutated(|value| {
        value["relations"].as_array_mut().unwrap().swap(0, 1);
    })
    .is_err());
}

#[test]
fn root_flags_sources_generations_and_nested_shapes_are_strict() {
    assert!(
        parse_mutated(|value| value["relations"][0]["scope_root"] = Value::Bool(false)).is_err()
    );
    assert!(
        parse_mutated(|value| value["relations"][1]["scope_root"] = Value::Bool(true)).is_err()
    );
    assert!(parse_mutated(|value| value["relations"][0]["generation"] = json!(0)).is_err());
    assert!(parse_mutated(
        |value| value["relations"][0]["generation"] = json!(9_007_199_254_740_992_u64)
    )
    .is_err());
    assert!(
        parse_mutated(|value| value["relations"][0]["source"]["adapter_id"] = json!("other"))
            .is_err()
    );
    assert!(parse_mutated(|value| value["relations"][0]["future"] = json!(true)).is_err());
    assert!(parse_mutated(|value| value["relations"][0]["state"]["future"] = json!(true)).is_err());
}

#[test]
fn decode_evidence_and_revision_cannot_be_retargeted() {
    assert!(parse_mutated(|value| value["relations"][0]["state"] =
        json!({ "kind": "absent", "absence_kind": "absent" }))
    .is_err());
    assert!(
        parse_mutated(|value| value["relations"][1]["source"]["object_key"] =
            value["relations"][0]["source"]["object_key"].clone())
        .is_err()
    );
    assert!(
        parse_mutated(|value| value["scope_revision"] = json!(encode_opaque(&[9; 32]))).is_err()
    );
    assert!(parse_mutated(|value| value["completeness"] = json!("partial")).is_err());
}

#[test]
fn source_cursor_authority_remains_exclusively_on_decode_coverage() {
    let (root, mut source_coverage, scope) = fixture_values();
    let revision = scope.scope_revision();
    source_coverage[0].points[0].position = Some(
        CoveragePosition::derive(
            CoveragePositionKind::AppendCursor,
            b"later-fixture-cursor",
            Some(8),
        )
        .unwrap(),
    );
    let context =
        ScopedScopeCoverageConsumerContext::from_expected(&scope, &root, &source_coverage).unwrap();
    assert_eq!(scope.scope_revision(), revision);
    assert!(ScopedScopeCoverageWire::from_expected(&scope, &context).is_ok());
}

#[test]
fn portable_projection_contains_no_native_locator_or_readiness_claim() {
    let (root, source_coverage, scope) = fixture_values();
    let context =
        ScopedScopeCoverageConsumerContext::from_expected(&scope, &root, &source_coverage).unwrap();
    let serialized = serde_json::to_string(&fixture_value()).unwrap();
    let debug = format!("{context:?}");
    for forbidden in [
        "native_path",
        "relative_path",
        "locator_id",
        "native-session",
        "barrier_sequence",
        "root_present",
        "family_manifest",
        "ready",
    ] {
        assert!(!serialized.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
}
