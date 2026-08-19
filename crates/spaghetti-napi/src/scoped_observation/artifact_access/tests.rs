use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::TempDir;

use crate::adapter::{
    fixture_scoped_access_request as scoped_access_request, supported_fixture_registry_with_scope,
    AdapterId, ArtifactCapture, ArtifactMetadataEntry, ArtifactMetadataSnapshotFact,
    ArtifactObservationKind, CanonicalEntityKey, CanonicalSourceInstanceKey, ContractCompleteness,
    EntityKey, ExternalEntityRef, Fact, FactBatch, FactSemanticContext, NativeIdentity,
    NativeIdentityClaim, QualifiedTimestamp, QualifiedValue, QualifiedValueQuality,
    RawRetentionPolicy, TimestampQuality,
};
use crate::source::{
    AccessBudgetError, AccessObjectToken, AccessOutcome, AccessPhase, RecordOrigin, SourceCursor,
    SourceMediaType, SourceRecord,
};

use super::*;
use crate::scoped_observation::{
    scoped_record_evidence, ScopedArtifactContentPolicy, ScopedArtifactRelationGrant,
    ScopedDecodedAppendItem, ScopedObservationAccessHost, ScopedObservationAdmissionLane,
    ScopedObservationEpochState, ScopedObservationProjectionLimits, ScopedObservationQueueLimits,
    ScopedQueuedObservationFrame, ScopedRootIdentityRequest, ScopedSourceObjectIdentity,
};

const ARTIFACT_SCOPE_DOCUMENT: &[u8] = br#"{
  "schema_version": 1,
  "declaration_id": "fixture-artifact-scope",
  "adapter_id": "fixture",
  "ads_id": "fixture-ads",
  "status": "promoted",
  "roots": ["artifact", "root"],
  "programs": [{
    "program_id": "observe-session",
    "root_entity_kind": "session",
    "root_relation_id": "root-object",
    "relations": [
      {
        "relation_id": "root-object",
        "primitive": "KnownObject",
        "access_root": "root",
        "locator": "known-object",
        "identity_inputs": ["native-session-id"],
        "bounds": {"max_fan_out": 1, "max_depth": 1, "max_objects": 1, "max_bytes": 1024, "max_rows": 0},
        "unavailable_behavior": "record_unavailable",
        "claim_refs": ["scope-evidence"]
      },
      {
        "relation_id": "file-artifact",
        "primitive": "ArtifactLocatorFromEvidence",
        "access_root": "artifact",
        "locator": "file-history/{native-session-id}/{backup-name}",
        "identity_inputs": ["native-session-id", "backup-name", "artifact-version"],
        "bounds": {"max_fan_out": 4, "max_depth": 3, "max_objects": 4, "max_bytes": 4096, "max_rows": 0},
        "unavailable_behavior": "skip_optional",
        "claim_refs": ["scope-evidence"]
      },
      {
        "relation_id": "wrong-artifact-axis",
        "primitive": "ArtifactLocatorFromEvidence",
        "access_root": "artifact",
        "locator": "wrong-artifact-locator",
        "identity_inputs": ["native-session-id", "backup-name"],
        "bounds": {"max_fan_out": 1, "max_depth": 2, "max_objects": 1, "max_bytes": 1024, "max_rows": 0},
        "unavailable_behavior": "skip_optional",
        "claim_refs": ["scope-evidence"]
      },
      {
        "relation_id": "conceptual-artifact",
        "primitive": "ArtifactLocatorFromEvidence",
        "access_root": "artifact",
        "locator": "conceptual-artifact-locator",
        "identity_inputs": ["native-session-id", "backup-name", "artifact-version"],
        "bounds": {"max_fan_out": 1, "max_depth": 2, "max_objects": 1, "max_bytes": 1024, "max_rows": 0},
        "unavailable_behavior": "skip_optional",
        "claim_refs": ["scope-evidence"]
      }
    ],
    "claim_refs": ["scope-evidence"]
  }],
  "blockers": [],
  "claim_refs": ["scope-evidence"]
}"#;

fn exact_root_identity(with_native_claim: bool) -> ScopedRootIdentityRequest {
    let adapter_id = AdapterId::new("fixture").unwrap();
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"fixture-source-instance").unwrap();
    let session_key = CanonicalEntityKey::derive(
        adapter_id.as_str(),
        &source_instance_key,
        "session",
        b"fixture-session",
    )
    .unwrap();
    let session_ref = ExternalEntityRef::new(session_key);
    let request = ScopedRootIdentityRequest::new(
        1,
        b"fixture-source-instance".as_slice(),
        b"fixture-session".as_slice(),
        None,
        Some(session_key),
        Some(session_ref),
    );
    if !with_native_claim {
        return request;
    }
    let identity = QualifiedValue::from_parts(
        Some(NativeIdentity {
            native_namespace: "fixture.session".to_string(),
            native_id: "native-session".to_string(),
        }),
        QualifiedValueQuality::NativeClaimed,
        "fixture".to_string(),
        ContractCompleteness::Complete,
        None,
        None,
        Vec::new(),
    )
    .unwrap();
    request.with_native_session_claim(NativeIdentityClaim::new(session_ref, identity).unwrap())
}

fn artifact_host(
    temp: &TempDir,
    suffix: &str,
    with_native_claim: bool,
) -> ScopedObservationAccessHost {
    let registry = supported_fixture_registry_with_scope(ARTIFACT_SCOPE_DOCUMENT);
    let known_root = temp.path().join(format!("{suffix}-known"));
    let artifact_root = temp.path().join(format!("{suffix}-artifacts"));
    let mut request = scoped_access_request(known_root);
    request.root_identity = exact_root_identity(with_native_claim);
    request
        .access_roots
        .push(super::super::ScopedAccessRootGrant {
            access_root: "artifact".to_string(),
            root: artifact_root,
        });
    request.artifact_relations = vec![ScopedArtifactRelationGrant {
        artifact_kind: "workflow_definition".to_string(),
        relation_id: "file-artifact".to_string(),
    }];
    ScopedObservationAccessHost::authorize(&registry, request).unwrap()
}

fn active_with_content_evidence(
    host: &ScopedObservationAccessHost,
) -> (ScopedObservationEpochState, CanonicalEntityKey) {
    let context = FactSemanticContext::new(
        &AdapterId::new("fixture").unwrap(),
        1,
        b"fixture-source-instance",
        b"fixture-transcript",
        b"session.jsonl",
        1,
    )
    .unwrap();
    let origin = RecordOrigin {
        source_instance_id: 11,
        stream_id: 12,
        object_id: 13,
        observed_at: 14,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
    };
    let record = SourceRecord::new(
        &origin,
        1,
        SourceCursor::append_offset(0),
        SourceCursor::append_offset(16),
        0,
        b"artifact-metadata".to_vec(),
    );
    let mut batch = FactBatch::new_with_semantic_context(4, 4, context.clone()).unwrap();
    let artifact_key = batch
        .canonical_entity_key("artifact", b"backup-17@v7")
        .unwrap();
    let fact = ArtifactMetadataSnapshotFact {
        session: EntityKey::native(
            &AdapterId::new("fixture").unwrap(),
            11,
            "session",
            b"fixture-session",
        )
        .unwrap(),
        canonical_session: Some(
            batch
                .canonical_entity_key("session", b"fixture-session")
                .unwrap(),
        ),
        native_message_id: "artifact-message".to_string(),
        native_snapshot_message_id: "artifact-snapshot".to_string(),
        observation_kind: ArtifactObservationKind::Checkpoint,
        is_snapshot_update: false,
        source_time: None,
        artifacts: vec![ArtifactMetadataEntry {
            artifact: EntityKey::native(
                &AdapterId::new("fixture").unwrap(),
                11,
                "artifact",
                b"backup-17@v7",
            )
            .unwrap(),
            canonical_artifact: Some(artifact_key),
            native_artifact_id: Some("backup-17@v7".to_string()),
            tracking_path: "/private/never/exposed.txt".to_string(),
            real_parent_dir: Some("/private/never".to_string()),
            version: 7,
            backup_time: QualifiedTimestamp {
                value: "2026-08-18T00:00:00Z".to_string(),
                quality: TimestampQuality::NativeExact,
            },
            capture: ArtifactCapture::ContentExpected,
        }],
    };
    batch
        .push_native(
            &record,
            b"artifact-message",
            Fact::ArtifactMetadataSnapshot(fact),
        )
        .unwrap();
    let source = ScopedSourceObjectIdentity::from_semantic_context(&context).unwrap();
    let frame = ScopedQueuedObservationFrame::Decoded {
        object_token: 71,
        source,
        lane_ordinal: 1,
        phase: super::super::ScopedAppendDeliveryPhase::Live,
        item: Box::new(ScopedDecodedAppendItem::Record {
            evidence: Box::new(scoped_record_evidence(&record, RawRetentionPolicy::None)),
            disposition: crate::adapter::DecodeDisposition::Applied,
            batch,
            quarantined: false,
        }),
    };
    let mut projection = host
        .open_projection_sink(ScopedObservationProjectionLimits {
            max_usage_v2_entities: 1,
        })
        .unwrap();
    projection.project(&frame).unwrap();
    let admission = ScopedObservationAdmissionLane::new(ScopedObservationQueueLimits {
        max_data_events: 1,
        max_retained_native_bytes: 0,
        max_control_items: 1,
        max_coverage_objects: 1,
    })
    .unwrap();
    (
        ScopedObservationEpochState {
            attachment_authority: Arc::clone(&host.attachment_authority),
            root: host.root_identity.clone(),
            scope_epoch: 1,
            append_objects: BTreeMap::new(),
            admission,
            projection,
            object_errors: BTreeMap::new(),
        },
        artifact_key,
    )
}

#[test]
fn exact_evidence_relation_reservation_is_bound_redacted_and_conservative() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "first", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    let command = host
        .prepare_portable_artifact_read_from_evidence(
            &active,
            artifact_key,
            "workflow_definition",
            Some(91),
            512,
            ScopedArtifactContentPolicy::HashOnly,
        )
        .unwrap();
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let pass = host.begin_pass().unwrap();
    let proof = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap();

    let expected = AccessObjectToken::derive(
        "file-artifact",
        &[
            b"native-session-id",
            b"native-session",
            b"backup-name",
            b"backup-17@v7",
            b"artifact-version",
            b"7",
        ],
    )
    .unwrap();
    assert_eq!(proof.object_token, expected);
    assert_eq!(proof.relation_id.as_ref(), "file-artifact");
    assert_eq!(proof.access_root.as_ref(), "artifact");
    assert_eq!(
        proof.locator_id.as_ref(),
        "file-history/{native-session-id}/{backup-name}"
    );
    assert_eq!(
        proof._relative_path,
        std::path::PathBuf::from("file-history/native-session/backup-17@v7")
    );
    assert_eq!(proof._native_session_id.as_ref(), "native-session");
    assert_eq!(proof._native_artifact_id.as_ref(), "backup-17@v7");
    assert_eq!(proof._artifact_version.as_ref(), "7");
    let debug = format!("{proof:?}");
    for secret in [
        temp.path().to_string_lossy().as_ref(),
        "/native-session/",
        "backup-17@v7",
        "/private/never/exposed.txt",
    ] {
        assert!(!debug.contains(secret));
    }

    let relation = pass
        .report()
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap()
        .clone();
    assert_eq!(relation.reservations_granted, 1);
    assert_eq!(relation.completed, 0);
    assert_eq!(relation.abandoned, 0);
    assert_eq!(relation.trace.len(), 0);
    drop(proof);
    let relation = pass
        .report()
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap()
        .clone();
    assert_eq!(relation.abandoned, 1);
    assert_eq!(relation.trace.len(), 1);
    assert_eq!(relation.trace[0].phase, AccessPhase::Revalidation);
    assert_eq!(relation.trace[0].outcome, AccessOutcome::Abandoned);
    assert_eq!(relation.trace[0].parent_token, None);
    assert_eq!(relation.trace[0].object_token, expected);
    let trace = serde_json::to_string(&relation).unwrap();
    assert!(!trace.contains("native-session"));
    assert!(!trace.contains("backup-17@v7"));
}

#[test]
fn relation_roots_axes_native_identity_and_attachment_fail_closed() {
    let registry = supported_fixture_registry_with_scope(ARTIFACT_SCOPE_DOCUMENT);
    let temp = TempDir::new().unwrap();

    let mut missing_root = scoped_access_request(temp.path().join("missing-known"));
    missing_root.root_identity = exact_root_identity(true);
    missing_root.artifact_relations = vec![ScopedArtifactRelationGrant {
        artifact_kind: "workflow_definition".to_string(),
        relation_id: "file-artifact".to_string(),
    }];
    assert!(matches!(
        ScopedObservationAccessHost::authorize(&registry, missing_root),
        Err(ScopedObservationAccessError::InvalidGrant(_))
    ));

    let mut mismatched_known_root = scoped_access_request(temp.path().join("mismatched-known"));
    mismatched_known_root.root_identity = exact_root_identity(true);
    mismatched_known_root.access_roots[0].root = temp.path().join("different-known-root");
    assert!(matches!(
        ScopedObservationAccessHost::authorize(&registry, mismatched_known_root),
        Err(ScopedObservationAccessError::InvalidGrant(_))
    ));

    let mut wrong_axis = scoped_access_request(temp.path().join("wrong-axis-known"));
    wrong_axis.root_identity = exact_root_identity(true);
    wrong_axis.access_roots.push(ScopedAccessRootGrant {
        access_root: "artifact".to_string(),
        root: temp.path().join("wrong-axis-artifacts"),
    });
    wrong_axis.artifact_relations = vec![ScopedArtifactRelationGrant {
        artifact_kind: "workflow_definition".to_string(),
        relation_id: "wrong-artifact-axis".to_string(),
    }];
    assert!(matches!(
        ScopedObservationAccessHost::authorize(&registry, wrong_axis),
        Err(ScopedObservationAccessError::InvalidGrant(_))
    ));

    let mut conceptual = scoped_access_request(temp.path().join("conceptual-known"));
    conceptual.root_identity = exact_root_identity(true);
    conceptual.access_roots.push(ScopedAccessRootGrant {
        access_root: "artifact".to_string(),
        root: temp.path().join("conceptual-artifacts"),
    });
    conceptual.artifact_relations = vec![ScopedArtifactRelationGrant {
        artifact_kind: "workflow_definition".to_string(),
        relation_id: "conceptual-artifact".to_string(),
    }];
    assert!(matches!(
        ScopedObservationAccessHost::authorize(&registry, conceptual),
        Err(ScopedObservationAccessError::InvalidGrant(_))
    ));

    let mut duplicate = scoped_access_request(temp.path().join("duplicate-known"));
    duplicate.root_identity = exact_root_identity(true);
    duplicate.access_roots.push(ScopedAccessRootGrant {
        access_root: "artifact".to_string(),
        root: temp.path().join("duplicate-artifacts"),
    });
    duplicate.artifact_relations = vec![
        ScopedArtifactRelationGrant {
            artifact_kind: "workflow_definition".to_string(),
            relation_id: "file-artifact".to_string(),
        },
        ScopedArtifactRelationGrant {
            artifact_kind: "tracked_file".to_string(),
            relation_id: "file-artifact".to_string(),
        },
    ];
    assert!(matches!(
        ScopedObservationAccessHost::authorize(&registry, duplicate),
        Err(ScopedObservationAccessError::InvalidGrant(_))
    ));

    let no_native = artifact_host(&temp, "no-native", false);
    let (active, artifact_key) = active_with_content_evidence(&no_native);
    let command = no_native
        .prepare_portable_artifact_read_from_evidence(
            &active,
            artifact_key,
            "workflow_definition",
            None,
            128,
            ScopedArtifactContentPolicy::MetadataOnly,
        )
        .unwrap();
    let validated = no_native
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let pass = no_native.begin_pass().unwrap();
    assert!(matches!(
        pass.reserve_artifact_relation_from_evidence(validated),
        Err(ScopedArtifactRelationAccessError::NativeSessionUnavailable)
    ));
    let attempts = pass
        .report()
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap()
        .attempts;
    assert_eq!(attempts, 0);
    drop(pass);

    let over_budget = artifact_host(&temp, "over-budget", true);
    let (active, artifact_key) = active_with_content_evidence(&over_budget);
    let command = over_budget
        .prepare_portable_artifact_read_from_evidence(
            &active,
            artifact_key,
            "workflow_definition",
            None,
            4097,
            ScopedArtifactContentPolicy::HashOnly,
        )
        .unwrap();
    let validated = over_budget
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let pass = over_budget.begin_pass().unwrap();
    assert!(matches!(
        pass.reserve_artifact_relation_from_evidence(validated),
        Err(ScopedArtifactRelationAccessError::Access(
            AccessBudgetError::LimitExceeded {
                relation_id,
                limit: crate::source::AccessLimit::MaxBytes,
            }
        )) if relation_id == "file-artifact"
    ));
    let relation = pass
        .report()
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap()
        .clone();
    assert_eq!(relation.attempts, 1);
    assert_eq!(relation.denied, 1);
    assert_eq!(relation.reservations_granted, 0);
    assert_eq!(relation.trace[0].outcome, AccessOutcome::Denied);
    assert!(!serde_json::to_string(&relation)
        .unwrap()
        .contains("backup-17@v7"));
    drop(pass);

    let first = artifact_host(&temp, "foreign-first", true);
    let second = artifact_host(&temp, "foreign-second", true);
    let (active, artifact_key) = active_with_content_evidence(&first);
    let command = first
        .prepare_portable_artifact_read_from_evidence(
            &active,
            artifact_key,
            "workflow_definition",
            None,
            128,
            ScopedArtifactContentPolicy::MetadataOnly,
        )
        .unwrap();
    let validated = first
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let foreign_pass = second.begin_pass().unwrap();
    assert!(matches!(
        foreign_pass.reserve_artifact_relation_from_evidence(validated),
        Err(ScopedArtifactRelationAccessError::InvalidBinding)
    ));
    assert_eq!(
        foreign_pass
            .report()
            .relations()
            .iter()
            .map(|relation| relation.attempts)
            .sum::<u64>(),
        0
    );
}

#[test]
fn unmapped_artifact_kind_cannot_mint_an_evidence_bound_command() {
    let registry = supported_fixture_registry_with_scope(ARTIFACT_SCOPE_DOCUMENT);
    let temp = TempDir::new().unwrap();
    let mut request = scoped_access_request(temp.path().join("unmapped-known"));
    request.root_identity = exact_root_identity(true);
    let host = ScopedObservationAccessHost::authorize(&registry, request).unwrap();
    let (active, artifact_key) = active_with_content_evidence(&host);
    assert_eq!(
        host.prepare_portable_artifact_read_from_evidence(
            &active,
            artifact_key,
            "workflow_definition",
            None,
            128,
            ScopedArtifactContentPolicy::MetadataOnly,
        )
        .expect_err("unmapped kinds cannot mint production commands"),
        super::super::artifact_wire::ScopedArtifactContractError::RelationUnavailable
    );
}

#[test]
fn access_root_debug_and_selected_sets_do_not_disclose_native_paths() {
    let temp = TempDir::new().unwrap();
    let grant = ScopedAccessRootGrant {
        access_root: "artifact".to_string(),
        root: temp.path().join("private-artifacts"),
    };
    let debug = format!("{grant:?}");
    assert!(debug.contains("artifact"));
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains(temp.path().to_string_lossy().as_ref()));
}
