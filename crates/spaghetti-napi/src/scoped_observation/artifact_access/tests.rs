use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use tempfile::TempDir;

use crate::adapter::{
    fixture_scoped_access_request as scoped_access_request, supported_fixture_registry_with_scope,
    AdapterId, ArtifactCapture, ArtifactMetadataEntry, ArtifactMetadataSnapshotFact,
    ArtifactObservationKind, CanonicalEntityKey, CanonicalSourceInstanceKey, ContractCompleteness,
    EntityKey, ExternalEntityRef, Fact, FactBatch, FactSemanticContext, NativeIdentity,
    NativeIdentityClaim, QualifiedTimestamp, QualifiedValue, QualifiedValueQuality,
    RawRetentionPolicy, SourceRoot, TimestampQuality,
};
use crate::source::{
    AccessBudgetError, AccessObjectToken, AccessOutcome, AccessPhase, RecordOrigin, SourceCursor,
    SourceMediaType, SourceRecord,
};

use super::*;
use crate::scoped_observation::{
    scoped_record_evidence, ScopedArtifactContentPolicy, ScopedArtifactRelationGrant,
    ScopedDecodedAppendItem, ScopedObservationAccessHost, ScopedObservationAdmissionLane,
    ScopedObservationDeliveryLimits, ScopedObservationEpochState, ScopedObservationEvent,
    ScopedObservationKnownEventFamily, ScopedObservationProjectionLimits,
    ScopedObservationQueueLimits, ScopedProjectionError, ScopedQueuedObservationFrame,
    ScopedRootIdentityRequest, ScopedSourceObjectIdentity,
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
        "source_binding": {"stream_id": "artifact-blobs", "primitive": "ReplaceDocument", "max_object_bytes": 1024},
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
        "source_binding": {"stream_id": "artifact-blobs", "primitive": "ReplaceDocument", "max_object_bytes": 1024},
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
        "source_binding": {"stream_id": "artifact-blobs", "primitive": "ReplaceDocument", "max_object_bytes": 1024},
        "unavailable_behavior": "skip_optional",
        "claim_refs": ["scope-evidence"]
      }
    ],
    "claim_refs": ["scope-evidence"]
  }],
  "blockers": [],
  "claim_refs": ["scope-evidence"]
}"#;

const ORDERED_AVAILABILITY_FIXTURE: &str = include_str!(
    "../../../fixtures/contracts/rfc012d-scoped-artifact-availability-envelope-v1.json"
);

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
    request.source_instance.spec.roots.push(SourceRoot {
        name: "artifact".to_string(),
        path: artifact_root.clone(),
    });
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
            mapping_disposition: Box::new(crate::scoped_observation::mapped_record_disposition(
                &batch,
            )),
            batch: Box::new(batch),
            scope_join_updates: Vec::new(),
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

fn artifact_command(
    host: &ScopedObservationAccessHost,
    active: &ScopedObservationEpochState,
    artifact_key: CanonicalEntityKey,
    expected_generation: Option<u64>,
    max_bytes: u64,
    content_policy: ScopedArtifactContentPolicy,
) -> super::super::artifact_wire::ScopedArtifactReadCommand {
    host.prepare_portable_artifact_read_from_evidence(
        active,
        artifact_key,
        "workflow_definition",
        expected_generation,
        max_bytes,
        content_policy,
    )
    .unwrap()
}

fn write_artifact(temp: &TempDir, suffix: &str, content: &[u8]) -> std::path::PathBuf {
    let path = temp
        .path()
        .join(format!("{suffix}-artifacts"))
        .join("file-history/native-session/backup-17@v7");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    path
}

fn observed_artifact(
    host: &ScopedObservationAccessHost,
    active: &ScopedObservationEpochState,
    pass: &ScopedObservationAccessPass,
    artifact_key: CanonicalEntityKey,
    expected_generation: Option<u64>,
    max_bytes: u64,
    content_policy: ScopedArtifactContentPolicy,
) -> Value {
    let command = artifact_command(
        host,
        active,
        artifact_key,
        expected_generation,
        max_bytes,
        content_policy,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(active, &command)
        .unwrap();
    let capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    serde_json::to_value(capture.into_observed_for_test().unwrap()).unwrap()
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
    assert_eq!(
        proof.source_binding.source_declaration_digest,
        *pass.plan.source_declaration_digest()
    );
    assert_eq!(proof.source_binding.max_object_bytes, 1024);
    assert_eq!(
        proof.source_binding.source.source_instance_key,
        active.root.source_instance_key
    );
    assert_eq!(
        proof.source_binding.source.stream_key,
        CoverageStreamKey::derive("fixture", b"artifact-blobs").unwrap()
    );
    assert_eq!(
        proof.source_binding.source.object_key,
        CoverageObjectKey::derive(
            "artifact-blobs",
            &confined_relative_path_key(std::path::Path::new(
                "file-history/native-session/backup-17@v7"
            ))
            .unwrap()
        )
        .unwrap()
    );
    let debug = format!("{proof:?}");
    for secret in [
        temp.path().to_string_lossy().as_ref(),
        "/native-session/",
        "backup-17@v7",
        "/private/never/exposed.txt",
    ] {
        assert!(!debug.contains(secret));
    }

    let oversized_command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        1025,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    let oversized_validated = host
        .validate_evidence_bound_artifact_command(&active, &oversized_command)
        .unwrap();
    assert!(matches!(
        pass.reserve_artifact_relation_from_evidence(oversized_validated),
        Err(ScopedArtifactRelationAccessError::InvalidBinding)
    ));

    let report = pass.report();
    let relation = report
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
fn confined_capture_applies_disclosure_policy_and_retains_generation_binding() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "capture", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    let content = b"echo bounded artifact\n";
    let path = write_artifact(&temp, "capture", content);
    let expected_hash = Sha256Digest::of(content);
    let expected_hash_text = expected_hash.to_string();
    let pass = host.begin_pass().unwrap();

    for (policy, expects_hash, expects_content) in [
        (ScopedArtifactContentPolicy::MetadataOnly, false, false),
        (ScopedArtifactContentPolicy::HashOnly, true, false),
        (ScopedArtifactContentPolicy::Inline, true, true),
    ] {
        let command = artifact_command(&host, &active, artifact_key, Some(91), 512, policy);
        let validated = host
            .validate_evidence_bound_artifact_command(&active, &command)
            .unwrap();
        let capture = pass
            .reserve_artifact_relation_from_evidence(validated)
            .unwrap()
            .read_confined()
            .unwrap();
        assert_eq!(capture._validated.expected_generation(), Some(91));
        let ScopedArtifactConfinedCaptureState::Stable {
            size_bytes,
            content_hash,
            content: retained_content,
            ..
        } = &capture.state
        else {
            panic!("stable artifact unexpectedly changed state");
        };
        assert_eq!(*size_bytes, content.len() as u64);
        assert_eq!(content_hash.is_some(), expects_hash);
        if expects_hash {
            assert_eq!(*content_hash, Some(expected_hash));
        }
        assert_eq!(retained_content.is_some(), expects_content);
        if expects_content {
            assert_eq!(retained_content.as_deref(), Some(content.as_slice()));
        }
        let debug = format!("{capture:?}");
        assert!(debug.contains("state: \"stable\""));
        assert!(debug.contains("native_path: \"<redacted>\""));
        for secret in [
            path.to_string_lossy().as_ref(),
            "echo bounded artifact",
            expected_hash_text.as_str(),
            "backup-17@v7",
        ] {
            assert!(!debug.contains(secret));
        }
        drop(capture);
    }

    let report = pass.report();
    let relation = report
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap()
        .clone();
    assert_eq!(relation.attempts, 3);
    assert_eq!(relation.reservations_granted, 3);
    assert_eq!(relation.completed, 3);
    assert_eq!(relation.bytes_read, (content.len() * 3) as u64);
    assert!(relation
        .trace
        .iter()
        .all(|entry| entry.outcome == AccessOutcome::Available));
}

#[test]
fn portable_capture_uses_attachment_local_replace_generation_without_retargeting() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "generation", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    let path = write_artifact(&temp, "generation", b"first revision\n");
    let pass = host.begin_pass().unwrap();

    let first = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        None,
        128,
        ScopedArtifactContentPolicy::Inline,
    );
    assert_eq!(first["outcome"]["kind"], "available");
    assert_eq!(first["outcome"]["generation"], 1);
    assert_eq!(first["outcome"]["size_bytes"], 15);
    let first_provenance = first["outcome"]["provenance_ref"]
        .as_str()
        .unwrap()
        .to_string();
    drop(pass);
    let pass = host.begin_pass().unwrap();

    let repeated = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        Some(1),
        128,
        ScopedArtifactContentPolicy::HashOnly,
    );
    assert_eq!(repeated["outcome"]["kind"], "available");
    assert_eq!(repeated["outcome"]["generation"], 1);
    assert_eq!(repeated["outcome"]["provenance_ref"], first_provenance);
    assert!(repeated["outcome"]["content_base64"].is_null());

    std::fs::write(&path, b"second revision\n").unwrap();
    let revised = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        Some(1),
        128,
        ScopedArtifactContentPolicy::HashOnly,
    );
    assert_eq!(revised["outcome"]["kind"], "available");
    assert_eq!(revised["outcome"]["generation"], 1);
    assert_ne!(revised["outcome"]["provenance_ref"], first_provenance);

    std::fs::remove_file(&path).unwrap();
    let deleted = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        Some(1),
        128,
        ScopedArtifactContentPolicy::Inline,
    );
    assert_eq!(deleted["outcome"]["kind"], "unavailable");
    assert_eq!(deleted["outcome"]["reason"], "changed_generation");
    assert_eq!(deleted["outcome"]["observed_generation"], 2);
    assert!(deleted["outcome"]["observed_bytes"].is_null());
    assert!(deleted["outcome"].get("content_base64").is_none());
    let deleted_provenance = deleted["outcome"]["provenance_ref"]
        .as_str()
        .unwrap()
        .to_string();

    let still_missing = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        Some(2),
        128,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    assert_eq!(still_missing["outcome"]["reason"], "missing");
    assert_eq!(still_missing["outcome"]["observed_generation"], 2);
    assert_eq!(
        still_missing["outcome"]["provenance_ref"],
        deleted_provenance
    );

    write_artifact(&temp, "generation", b"recreated\n");
    let recreated = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        Some(2),
        128,
        ScopedArtifactContentPolicy::Inline,
    );
    assert_eq!(recreated["outcome"]["reason"], "changed_generation");
    assert_eq!(recreated["outcome"]["observed_generation"], 3);
    assert!(recreated["outcome"].get("content_base64").is_none());

    let current = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        Some(3),
        128,
        ScopedArtifactContentPolicy::Inline,
    );
    assert_eq!(current["outcome"]["kind"], "available");
    assert_eq!(current["outcome"]["generation"], 3);
    assert_eq!(current["outcome"]["size_bytes"], 10);

    let encoded = serde_json::to_string(&[
        first,
        repeated,
        revised,
        deleted,
        still_missing,
        recreated,
        current,
    ])
    .unwrap();
    assert!(!encoded.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!encoded.contains("backup-17@v7"));
}

#[test]
fn completed_capture_updates_current_availability_without_disclosure_policy_drift() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "availability", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    let path = write_artifact(&temp, "availability", b"first revision\n");
    let initial = host.artifact_availability_snapshot(&active).unwrap();
    assert_eq!(initial.entry_count(), 0);

    let mut drain = host
        .open_consumer_drain(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
    let pass = host.begin_pass().unwrap();
    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        128,
        ScopedArtifactContentPolicy::Inline,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let mut capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    assert_eq!(
        host.artifact_availability_snapshot(&active)
            .unwrap()
            .entry_count(),
        0,
        "a native capture is not published before strict result construction"
    );
    let (observed, first_receipt) = capture.offer_observed(&mut drain, 1_000).unwrap();
    assert_eq!(
        serde_json::to_value(observed).unwrap()["outcome"]["kind"],
        "available"
    );
    let first_receipt = first_receipt.unwrap();
    assert_eq!(first_receipt.first_offered_sequence, Some(1));
    assert_eq!(first_receipt.offered_through_sequence, 1);
    let first = host.artifact_availability_snapshot(&active).unwrap();
    assert_eq!(first.entry_count(), 1);

    std::fs::write(&path, b"second revision\n").unwrap();
    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        Some(1),
        128,
        ScopedArtifactContentPolicy::HashOnly,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let mut revised_capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    assert_eq!(
        revised_capture
            .offer_observed(&mut drain, 2_000)
            .unwrap_err(),
        ScopedArtifactObservationOfferError::Delivery(ScopedDeliveryError::SemanticQueueFull)
    );
    assert!(!revised_capture.offered);
    assert_eq!(
        host.artifact_availability_snapshot(&active)
            .unwrap()
            .semantic_digest(),
        first.semantic_digest()
    );
    assert_eq!(drain.delivery_lane().state().offered_through_sequence, 1);
    assert_eq!(
        revised_capture
            .offer_observed(&mut drain, 2_001)
            .unwrap_err(),
        ScopedArtifactObservationOfferError::ObservationTimeDrift
    );
    assert_eq!(drain.delivery_lane().state().offered_through_sequence, 1);

    let yielded = drain.next().unwrap().unwrap();
    assert_eq!(yielded.envelope.observer_sequence, 1);
    assert_eq!(yielded.envelope.source.generation, 1);
    assert!(yielded.envelope.source.locator_id.is_none());
    assert!(yielded.envelope.source.source_record_id.is_none());
    assert!(yielded.envelope.source.cursor_start.is_none());
    assert!(yielded.envelope.source.cursor_end.is_none());
    assert!(matches!(
        &yielded.envelope.event,
        ScopedObservationEvent::ArtifactAvailability { entry }
            if entry.artifact_key() == artifact_key
                && matches!(
                    entry.state(),
                    ScopedArtifactAvailabilityState::Available {
                        generation: 1,
                        size_bytes: 15,
                        ..
                    }
                )
    ));
    drain
        .acknowledge_applied(yielded.application_receipt())
        .unwrap();

    let (_, revised_receipt) = revised_capture.offer_observed(&mut drain, 2_000).unwrap();
    let revised_receipt = revised_receipt.unwrap();
    assert_eq!(revised_receipt.first_offered_sequence, Some(2));
    assert_eq!(revised_receipt.offered_through_sequence, 2);
    let revised = host.artifact_availability_snapshot(&active).unwrap();
    assert_ne!(revised.semantic_digest(), first.semantic_digest());
    let yielded = drain.next().unwrap().unwrap();
    assert_eq!(yielded.envelope.observer_sequence, 2);
    drain
        .acknowledge_applied(yielded.application_receipt())
        .unwrap();

    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        Some(1),
        128,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let mut replay = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    let (_, replay_receipt) = replay.offer_observed(&mut drain, 3_000).unwrap();
    assert!(replay_receipt.is_none());
    assert_eq!(
        replay.offer_observed(&mut drain, 3_001).unwrap_err(),
        ScopedArtifactObservationOfferError::AlreadyOffered
    );
    let policy_replay = host.artifact_availability_snapshot(&active).unwrap();
    assert_eq!(policy_replay.semantic_digest(), revised.semantic_digest());
    assert_eq!(drain.delivery_lane().state().offered_through_sequence, 2);
    assert!(drain.next().unwrap().is_none());

    let debug = format!("{revised:?}");
    for secret in [
        temp.path().to_string_lossy().as_ref(),
        "backup-17@v7",
        "second revision",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn ordered_availability_portable_fixture_is_produced_by_the_confined_pipeline() {
    use crate::scoped_observation::artifact_availability_event_wire::ScopedArtifactAvailabilityEnvelopeWire;

    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "ordered-wire", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    let mut drain = host
        .open_consumer_drain(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
    let pass = host.begin_pass().unwrap();
    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        128,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let mut capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    let (_, receipt) = capture.offer_observed(&mut drain, 1_750_000_000).unwrap();
    assert!(receipt.is_some());
    let yielded = drain.next().unwrap().unwrap();
    let context = yielded.artifact_availability_context().unwrap();
    let wire =
        ScopedArtifactAvailabilityEnvelopeWire::from_scoped_for_context(&yielded.envelope, context)
            .unwrap();
    let wire_value = serde_json::to_value(&wire).unwrap();
    assert_eq!(
        yielded.known_envelope().unwrap().family(),
        ScopedObservationKnownEventFamily::ArtifactAvailability
    );
    assert_eq!(yielded.known_envelope().unwrap().wire_value(), &wire_value);
    assert_eq!(
        yielded.known_envelope().unwrap().context_value(),
        &serde_json::to_value(context.wire()).unwrap()
    );
    let parsed = ScopedArtifactAvailabilityEnvelopeWire::from_wire_value_for_context(
        wire_value.clone(),
        context,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), wire_value);

    let report = serde_json::json!({
        "fixture_contract_version": 1,
        "context": context.wire(),
        "event": wire,
        "expected": {
            "ordered": true,
            "rust_event_id_authority": "private_source_declaration_occurrence",
            "portable_event_id_authority": "exact_rust_issued_context",
            "native_evidence": "engine_control_only",
            "source_declaration_digest_disclosed": false,
            "snapshot_revision_contract": "unchanged_v1"
        }
    });
    let expected: Value = serde_json::from_str(ORDERED_AVAILABILITY_FIXTURE).unwrap();
    if expected != report {
        panic!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!encoded.contains("backup-17@v7"));
    assert!(!serde_json::to_string(&report["context"])
        .unwrap()
        .contains("source_declaration_digest"));
    assert!(!serde_json::to_string(&report["event"])
        .unwrap()
        .contains("source_declaration_digest"));
}

#[test]
fn ordered_availability_rejects_foreign_and_wrong_epoch_drains_without_mutation() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "ordered-binding", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    write_artifact(&temp, "ordered-binding", b"bound\n");
    let mut drain = host
        .open_consumer_drain(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
    let foreign_host = artifact_host(&temp, "ordered-foreign", true);
    let mut foreign_drain = foreign_host
        .open_consumer_drain(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
    let pass = host.begin_pass().unwrap();
    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        64,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let mut capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();

    assert_eq!(
        capture.offer_observed(&mut foreign_drain, 1).unwrap_err(),
        ScopedArtifactObservationOfferError::ForeignDrain
    );
    assert_eq!(
        host.artifact_availability_snapshot(&active)
            .unwrap()
            .entry_count(),
        0
    );
    assert_eq!(
        foreign_drain
            .delivery_lane()
            .state()
            .offered_through_sequence,
        0
    );

    drain.delivery.scope_epoch = 2;
    assert_eq!(
        capture.offer_observed(&mut drain, 2).unwrap_err(),
        ScopedArtifactObservationOfferError::InvalidEpoch
    );
    assert_eq!(
        host.artifact_availability_snapshot(&active)
            .unwrap()
            .entry_count(),
        0
    );
    assert_eq!(drain.delivery_lane().state().offered_through_sequence, 0);

    drain.delivery.scope_epoch = 1;
    let exact_declaration_digest = capture.source_binding.source_declaration_digest;
    capture.source_binding.source_declaration_digest = [88; 32];
    assert_eq!(
        capture.offer_observed(&mut drain, 3).unwrap_err(),
        ScopedArtifactObservationOfferError::InvalidSourceBinding
    );
    assert_eq!(drain.delivery_lane().state().offered_through_sequence, 0);
    capture.source_binding.source_declaration_digest = exact_declaration_digest;
    let (_, receipt) = capture.offer_observed(&mut drain, 3).unwrap();
    assert_eq!(receipt.unwrap().first_offered_sequence, Some(1));
    assert_eq!(
        host.artifact_availability_snapshot(&active)
            .unwrap()
            .entry_count(),
        1
    );
}

#[test]
fn generation_ledger_is_bounded_and_overflow_fails_without_mutation() {
    let mut ledger = ScopedArtifactGenerationLedger::new();
    for ordinal in 0..MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&(ordinal as u64).to_be_bytes());
        let token = AccessObjectToken::from_bytes(bytes);
        ledger.preflight(token).unwrap();
        assert_eq!(ledger.observe(token, true).unwrap(), Some(1));
    }
    let full_token = AccessObjectToken::from_bytes([0xff; 32]);
    assert!(matches!(
        ledger.preflight(full_token),
        Err(ScopedArtifactRelationAccessError::GenerationCapacityExhausted)
    ));
    assert!(!ledger.objects.contains_key(&full_token));

    let overflow_token = AccessObjectToken::from_bytes([0; 32]);
    ledger.objects.insert(
        overflow_token,
        ScopedArtifactGenerationEntry {
            generation: u64::MAX,
            present: true,
        },
    );
    assert!(matches!(
        ledger.observe(overflow_token, false),
        Err(ScopedArtifactRelationAccessError::GenerationExhausted)
    ));
    assert_eq!(
        ledger.objects.get(&overflow_token),
        Some(&ScopedArtifactGenerationEntry {
            generation: u64::MAX,
            present: true,
        })
    );
}

#[test]
fn confined_capture_accounts_missing_oversized_and_path_escape_without_disclosure() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "bounds", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    let mut drain = host
        .open_consumer_drain(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
    let pass = host.begin_pass().unwrap();

    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        8,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let mut capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    assert!(matches!(
        &capture.state,
        ScopedArtifactConfinedCaptureState::Missing { .. }
    ));
    let (missing, receipt) = capture.offer_observed(&mut drain, 1_000).unwrap();
    let missing = serde_json::to_value(missing).unwrap();
    assert_eq!(missing["outcome"]["reason"], "missing");
    assert!(missing["outcome"]["observed_generation"].is_null());
    assert!(missing["outcome"]["provenance_ref"].is_null());
    assert_eq!(receipt.unwrap().first_offered_sequence, Some(1));
    let yielded = drain.next().unwrap().unwrap();
    assert_eq!(yielded.envelope.source.generation, 1);
    assert!(matches!(
        &yielded.envelope.event,
        ScopedObservationEvent::ArtifactAvailability { entry }
            if matches!(
                entry.state(),
                ScopedArtifactAvailabilityState::Missing {
                    observed_generation: None,
                    provenance_ref: None,
                }
            )
    ));
    drain
        .acknowledge_applied(yielded.application_receipt())
        .unwrap();

    let path = write_artifact(&temp, "bounds", b"123456789");
    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        8,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    assert!(matches!(
        &capture.state,
        ScopedArtifactConfinedCaptureState::Oversized {
            observed_bytes: 9,
            ..
        }
    ));
    let oversized = serde_json::to_value(capture.into_observed_for_test().unwrap()).unwrap();
    assert_eq!(oversized["outcome"]["reason"], "over_limit");
    assert_eq!(oversized["outcome"]["observed_generation"], 1);
    assert_eq!(oversized["outcome"]["observed_bytes"], 9);
    assert!(oversized["outcome"]["provenance_ref"].is_string());

    let changed = observed_artifact(
        &host,
        &active,
        &pass,
        artifact_key,
        Some(2),
        8,
        ScopedArtifactContentPolicy::MetadataOnly,
    );
    assert_eq!(changed["outcome"]["reason"], "changed_generation");
    assert_eq!(changed["outcome"]["observed_generation"], 1);
    assert!(changed["outcome"]["observed_bytes"].is_null());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = temp.path().join("private-outside");
        std::fs::write(&outside, b"private").unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&outside, &path).unwrap();
        let command = artifact_command(
            &host,
            &active,
            artifact_key,
            None,
            8,
            ScopedArtifactContentPolicy::MetadataOnly,
        );
        let validated = host
            .validate_evidence_bound_artifact_command(&active, &command)
            .unwrap();
        let error = pass
            .reserve_artifact_relation_from_evidence(validated)
            .unwrap()
            .read_confined()
            .unwrap_err();
        assert!(matches!(
            error,
            ScopedArtifactRelationAccessError::Source(ScopedSourceFailureClass::PathEscape)
        ));
        let debug = format!("{error:?}");
        assert!(!debug.contains(temp.path().to_string_lossy().as_ref()));
        assert!(!debug.contains("backup-17@v7"));
    }

    let relation = pass
        .report()
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap()
        .clone();
    #[cfg(unix)]
    assert_eq!(relation.attempts, 4);
    #[cfg(not(unix))]
    assert_eq!(relation.attempts, 3);
    assert_eq!(relation.abandoned, 0);
    assert_eq!(relation.trace[0].outcome, AccessOutcome::Unavailable);
    assert_eq!(relation.trace[1].outcome, AccessOutcome::Oversized);
    assert_eq!(relation.trace[2].outcome, AccessOutcome::Oversized);
    #[cfg(unix)]
    {
        assert_eq!(relation.trace[3].outcome, AccessOutcome::Failed);
        assert_eq!(relation.bytes_read, 8);
    }
    #[cfg(not(unix))]
    assert_eq!(relation.bytes_read, 0);
    let trace = serde_json::to_string(&relation).unwrap();
    assert!(!trace.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!trace.contains("backup-17@v7"));
}

#[test]
fn confined_capture_discards_a_read_if_the_attachment_closed_before_open() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "closed", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    write_artifact(&temp, "closed", b"not read");
    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        64,
        ScopedArtifactContentPolicy::Inline,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let pass = host.begin_pass().unwrap();
    let proof = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap();
    host.close();
    assert!(matches!(
        proof.read_confined(),
        Err(ScopedArtifactRelationAccessError::Closed)
    ));
    let report = pass.report();
    let relation = report
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap();
    assert_eq!(relation.bytes_read, 0);
    assert_eq!(relation.completed, 1);
    assert_eq!(relation.trace[0].outcome, AccessOutcome::Unavailable);
}

#[test]
fn confined_capture_cannot_publish_after_attachment_close() {
    let temp = TempDir::new().unwrap();
    let host = artifact_host(&temp, "close-after-read", true);
    let (active, artifact_key) = active_with_content_evidence(&host);
    write_artifact(&temp, "close-after-read", b"read then discard");
    let mut drain = host
        .open_consumer_drain(ScopedObservationDeliveryLimits {
            max_semantic_events: 1,
            max_retained_native_bytes: 0,
            max_source_control_items: 1,
        })
        .unwrap();
    let command = artifact_command(
        &host,
        &active,
        artifact_key,
        None,
        64,
        ScopedArtifactContentPolicy::Inline,
    );
    let validated = host
        .validate_evidence_bound_artifact_command(&active, &command)
        .unwrap();
    let pass = host.begin_pass().unwrap();
    let mut capture = pass
        .reserve_artifact_relation_from_evidence(validated)
        .unwrap()
        .read_confined()
        .unwrap();
    host.close();
    assert_eq!(
        capture.offer_observed(&mut drain, 1).unwrap_err(),
        ScopedArtifactObservationOfferError::Closed
    );
    assert_eq!(drain.delivery_lane().state().offered_through_sequence, 0);
    assert_eq!(
        host.artifact_availability_snapshot(&active).unwrap_err(),
        ScopedProjectionError::InvalidLifecycle
    );
    let report = pass.report();
    let relation = report
        .relations()
        .iter()
        .find(|relation| relation.relation_id == "file-artifact")
        .unwrap();
    assert_eq!(relation.bytes_read, 17);
    assert_eq!(relation.trace[0].outcome, AccessOutcome::Available);
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
