use crate::adapter::{
    AdapterId, ArtifactCapture, ArtifactMetadataEntry, ArtifactMetadataSnapshotFact,
    DecodeDisposition, EntityKey, Fact, FactBatch, FactSemanticContext, QualifiedTimestamp,
    RawRetentionPolicy, TimestampQuality,
};
use crate::source::{RecordOrigin, SourceCursor, SourceMediaType, SourceRecord};

use super::*;
use crate::scoped_observation::{
    scoped_record_evidence, ScopedAppendDeliveryPhase, ScopedAppendReset, ScopedDecodedAppendItem,
    ScopedObservationProjectionLimits, ScopedObservationProjectionSink, ScopedProjectionFamilies,
    ScopedQueuedObservationFrame, ScopedSourceObjectIdentity,
};
use crate::source::AppendTransition;

const OBJECT_TOKEN: u64 = 41;

fn semantic_context() -> FactSemanticContext {
    FactSemanticContext::new(
        &AdapterId::new("fixture").unwrap(),
        1,
        b"stable-source-instance",
        b"transcript",
        b"root-session.jsonl",
        1,
    )
    .unwrap()
}

fn record(
    source_instance_id: u64,
    stream_id: u64,
    object_id: u64,
    start: u64,
    end: u64,
    observed_at: i64,
) -> SourceRecord {
    SourceRecord::new(
        &RecordOrigin {
            source_instance_id,
            stream_id,
            object_id,
            observed_at,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        },
        1,
        SourceCursor::append_offset(start),
        SourceCursor::append_offset(end),
        0,
        format!("metadata-{start}-{end}").into_bytes(),
    )
}

fn metadata_fact(
    batch: &FactBatch,
    native_message_id: &str,
    canonical_artifact: CanonicalEntityKey,
    native_artifact_id: Option<&str>,
    capture: ArtifactCapture,
    version: u64,
) -> ArtifactMetadataSnapshotFact {
    ArtifactMetadataSnapshotFact {
        session: EntityKey::native(
            &AdapterId::new("fixture").unwrap(),
            17,
            "session",
            b"native-session",
        )
        .unwrap(),
        canonical_session: Some(
            batch
                .canonical_entity_key("session", b"native-session")
                .unwrap(),
        ),
        native_message_id: native_message_id.to_owned(),
        native_snapshot_message_id: format!("snapshot-{native_message_id}"),
        observation_kind: crate::adapter::ArtifactObservationKind::Checkpoint,
        is_snapshot_update: false,
        source_time: None,
        artifacts: vec![ArtifactMetadataEntry {
            artifact: EntityKey::native(
                &AdapterId::new("fixture").unwrap(),
                17,
                "artifact",
                native_message_id.as_bytes(),
            )
            .unwrap(),
            canonical_artifact: Some(canonical_artifact),
            native_artifact_id: native_artifact_id.map(str::to_owned),
            tracking_path: "/private/worktree/secret.txt".to_owned(),
            real_parent_dir: Some("/private/worktree".to_owned()),
            version,
            backup_time: QualifiedTimestamp {
                value: "2026-08-18T00:00:00Z".to_owned(),
                quality: TimestampQuality::NativeExact,
            },
            capture,
        }],
    }
}

fn batch_with_metadata(
    record: &SourceRecord,
    native_message_id: &str,
    canonical_artifact: CanonicalEntityKey,
    native_artifact_id: Option<&str>,
    capture: ArtifactCapture,
    version: u64,
) -> FactBatch {
    let mut batch = FactBatch::new_with_semantic_context(8, 4, semantic_context()).unwrap();
    let fact = metadata_fact(
        &batch,
        native_message_id,
        canonical_artifact,
        native_artifact_id,
        capture,
        version,
    );
    batch
        .push_native(
            record,
            native_message_id.as_bytes(),
            Fact::ArtifactMetadataSnapshot(fact),
        )
        .unwrap();
    batch
}

fn root_session() -> CanonicalEntityKey {
    CanonicalEntityKey::derive(
        "fixture",
        &semantic_context().source_instance_key(),
        "session",
        b"native-session",
    )
    .unwrap()
}

fn artifact_key(native: &[u8]) -> CanonicalEntityKey {
    CanonicalEntityKey::derive(
        "fixture",
        &semantic_context().source_instance_key(),
        "artifact",
        native,
    )
    .unwrap()
}

fn admit(
    reducer: &mut ScopedArtifactEvidenceReducer,
    source_record: &SourceRecord,
    batch: &FactBatch,
) -> Result<(), ScopedProjectionError> {
    let mut mutation = ScopedArtifactEvidenceMutation::default();
    let source = ScopedSourceObjectIdentity::from_semantic_context(&semantic_context()).unwrap();
    let evidence = scoped_record_evidence(source_record, RawRetentionPolicy::None);
    let envelope = &batch.facts()[0];
    let Fact::ArtifactMetadataSnapshot(fact) = &envelope.value else {
        panic!("expected artifact metadata");
    };
    reducer.prepare_metadata(
        &mut mutation,
        OBJECT_TOKEN,
        &source,
        &evidence,
        envelope,
        fact,
    )?;
    reducer.commit(mutation);
    Ok(())
}

#[test]
fn canonical_snapshot_is_topology_invariant_path_free_and_explicit() {
    let named = artifact_key(b"backup-a@v1");
    let uncaptured = artifact_key(b"uncaptured-a");
    let first_record = record(11, 22, 33, 0, 10, 44);
    let second_record = record(11, 22, 33, 10, 20, 45);

    let mut first = ScopedArtifactEvidenceReducer::new(Some(root_session()));
    admit(
        &mut first,
        &first_record,
        &batch_with_metadata(
            &first_record,
            "message-a",
            named,
            Some("backup-a@v1"),
            ArtifactCapture::ContentExpected,
            1,
        ),
    )
    .unwrap();
    admit(
        &mut first,
        &second_record,
        &batch_with_metadata(
            &second_record,
            "message-b",
            uncaptured,
            None,
            ArtifactCapture::NotCaptured,
            1,
        ),
    )
    .unwrap();
    let snapshot = first.snapshot().unwrap();
    assert_eq!(snapshot.entry_count, 2);
    assert_eq!(snapshot.entries[0].artifact_key, named.min(uncaptured));
    assert!(snapshot.entries.iter().any(|entry| {
        entry.artifact_key == named
            && entry.disposition == ScopedArtifactEvidenceDisposition::ContentExpected
    }));
    assert!(snapshot.entries.iter().any(|entry| {
        entry.artifact_key == uncaptured
            && entry.disposition == ScopedArtifactEvidenceDisposition::NotCaptured
    }));
    let debug = format!("{snapshot:?}");
    assert!(!debug.contains("secret.txt"));
    assert!(!debug.contains("backup-a@v1"));

    let alternate_first = record(111, 222, 333, 0, 10, 4_400);
    let alternate_second = record(111, 222, 333, 10, 20, 4_500);
    let mut alternate = ScopedArtifactEvidenceReducer::new(Some(root_session()));
    admit(
        &mut alternate,
        &alternate_second,
        &batch_with_metadata(
            &alternate_second,
            "message-b",
            uncaptured,
            None,
            ArtifactCapture::NotCaptured,
            1,
        ),
    )
    .unwrap();
    admit(
        &mut alternate,
        &alternate_first,
        &batch_with_metadata(
            &alternate_first,
            "message-a",
            named,
            Some("backup-a@v1"),
            ArtifactCapture::ContentExpected,
            1,
        ),
    )
    .unwrap();
    assert_eq!(snapshot, alternate.snapshot().unwrap());
}

#[test]
fn evidence_revisions_change_without_rekeying_and_conflicts_do_not_fabricate_availability() {
    let artifact = artifact_key(b"shared-artifact");
    let first_record = record(11, 22, 33, 0, 10, 44);
    let second_record = record(11, 22, 33, 10, 20, 45);
    let third_record = record(11, 22, 33, 20, 30, 46);
    let mut reducer = ScopedArtifactEvidenceReducer::new(Some(root_session()));
    admit(
        &mut reducer,
        &first_record,
        &batch_with_metadata(
            &first_record,
            "message-a",
            artifact,
            Some("backup-a@v1"),
            ArtifactCapture::ContentExpected,
            1,
        ),
    )
    .unwrap();
    let initial = reducer.snapshot().unwrap();

    admit(
        &mut reducer,
        &second_record,
        &batch_with_metadata(
            &second_record,
            "message-a",
            artifact,
            Some("backup-a@v1"),
            ArtifactCapture::ContentExpected,
            2,
        ),
    )
    .unwrap();
    let revised = reducer.snapshot().unwrap();
    assert_eq!(revised.entry_count, 1);
    assert_eq!(revised.entries[0].artifact_key, artifact);
    assert_eq!(revised.entries[0].evidence_count, 1);
    assert_eq!(
        revised.entries[0].disposition,
        ScopedArtifactEvidenceDisposition::ContentExpected
    );
    assert_ne!(
        initial.semantic_digest.as_bytes(),
        revised.semantic_digest.as_bytes()
    );

    admit(
        &mut reducer,
        &third_record,
        &batch_with_metadata(
            &third_record,
            "message-b",
            artifact,
            None,
            ArtifactCapture::NotCaptured,
            2,
        ),
    )
    .unwrap();
    let conflicted = reducer.snapshot().unwrap();
    assert_eq!(conflicted.entry_count, 1);
    assert_eq!(conflicted.entries[0].artifact_key, artifact);
    assert_eq!(
        conflicted.entries[0].disposition,
        ScopedArtifactEvidenceDisposition::Conflicting
    );
    assert_ne!(
        revised.semantic_digest.as_bytes(),
        conflicted.semantic_digest.as_bytes()
    );
}

#[test]
fn content_selection_is_exact_current_and_never_upgrades_conflicting_evidence() {
    let artifact = artifact_key(b"selected-artifact");
    let first_record = record(11, 22, 33, 0, 10, 44);
    let second_record = record(11, 22, 33, 10, 20, 45);
    let third_record = record(11, 22, 33, 20, 30, 46);
    let mut reducer = ScopedArtifactEvidenceReducer::new(Some(root_session()));
    admit(
        &mut reducer,
        &first_record,
        &batch_with_metadata(
            &first_record,
            "message-a",
            artifact,
            Some("selected-artifact@v1"),
            ArtifactCapture::ContentExpected,
            1,
        ),
    )
    .unwrap();
    let first = reducer
        .select_content_expected(artifact)
        .unwrap()
        .expect("content-bearing metadata selects the artifact");
    assert_eq!(first.root_session(), root_session());
    assert_eq!(first.artifact_key(), artifact);
    assert_eq!(first.native_artifact_id().as_ref(), "selected-artifact@v1");
    assert_eq!(first.version(), 1);
    assert!(reducer.selection_is_current(&first).unwrap());
    assert!(!format!("{first:?}").contains("selected-artifact@v1"));

    admit(
        &mut reducer,
        &second_record,
        &batch_with_metadata(
            &second_record,
            "message-a",
            artifact,
            Some("selected-artifact@v2"),
            ArtifactCapture::ContentExpected,
            2,
        ),
    )
    .unwrap();
    assert!(!reducer.selection_is_current(&first).unwrap());
    let revised = reducer
        .select_content_expected(artifact)
        .unwrap()
        .expect("the corrected evidence remains selectable");
    assert_eq!(revised.version(), 2);
    assert_eq!(
        revised.native_artifact_id().as_ref(),
        "selected-artifact@v2"
    );
    assert_ne!(first.revision(), revised.revision());

    admit(
        &mut reducer,
        &third_record,
        &batch_with_metadata(
            &third_record,
            "message-b",
            artifact,
            None,
            ArtifactCapture::NotCaptured,
            2,
        ),
    )
    .unwrap();
    assert!(reducer.select_content_expected(artifact).unwrap().is_none());
    assert!(!reducer.selection_is_current(&revised).unwrap());
}

#[test]
fn root_mismatch_and_source_lifecycle_fail_closed() {
    let artifact = artifact_key(b"backup-a@v1");
    let source_record = record(11, 22, 33, 0, 10, 44);
    let batch = batch_with_metadata(
        &source_record,
        "message-a",
        artifact,
        Some("backup-a@v1"),
        ArtifactCapture::ContentExpected,
        1,
    );

    let foreign_root = CanonicalEntityKey::derive(
        "fixture",
        &semantic_context().source_instance_key(),
        "session",
        b"foreign-session",
    )
    .unwrap();
    let mut foreign = ScopedArtifactEvidenceReducer::new(Some(foreign_root));
    assert_eq!(
        admit(&mut foreign, &source_record, &batch),
        Err(ScopedProjectionError::InvalidArtifactEvidence)
    );

    let mut reducer = ScopedArtifactEvidenceReducer::new(Some(root_session()));
    admit(&mut reducer, &source_record, &batch).unwrap();
    assert!(reducer.has_object(OBJECT_TOKEN));
    assert!(matches!(
        reducer.prepare_object_retractions(
            OBJECT_TOKEN,
            2,
            ScopedProjectionError::InvalidResetState,
        ),
        Err(ScopedProjectionError::InvalidResetState)
    ));
    let mutation = reducer
        .prepare_object_retractions(OBJECT_TOKEN, 1, ScopedProjectionError::InvalidResetState)
        .unwrap();
    reducer.commit(mutation);
    assert!(!reducer.has_object(OBJECT_TOKEN));
    assert_eq!(reducer.snapshot().unwrap().entry_count, 0);
}

#[test]
fn projection_commit_and_source_reset_move_artifact_evidence_atomically() {
    let source_record = record(11, 22, 33, 0, 10, 44);
    let batch = batch_with_metadata(
        &source_record,
        "message-a",
        artifact_key(b"backup-a@v1"),
        Some("backup-a@v1"),
        ArtifactCapture::ContentExpected,
        1,
    );
    let source = ScopedSourceObjectIdentity::from_semantic_context(&semantic_context()).unwrap();
    let mut sink = ScopedObservationProjectionSink::new_with_families(
        ScopedObservationProjectionLimits {
            max_usage_v2_entities: 8,
        },
        ScopedProjectionFamilies::usage_v2_only(),
        Some(root_session()),
    )
    .unwrap();
    let projected = sink
        .project(&ScopedQueuedObservationFrame::Decoded {
            object_token: OBJECT_TOKEN,
            source: source.clone(),
            lane_ordinal: 1,
            phase: ScopedAppendDeliveryPhase::Bootstrap,
            item: Box::new(ScopedDecodedAppendItem::Record {
                evidence: Box::new(scoped_record_evidence(
                    &source_record,
                    RawRetentionPolicy::None,
                )),
                disposition: DecodeDisposition::Applied,
                batch,
                quarantined: false,
            }),
        })
        .unwrap();
    assert!(projected.is_empty());
    assert_eq!(sink.artifact_evidence_snapshot().unwrap().entry_count, 1);

    let projected = sink
        .project(&ScopedQueuedObservationFrame::Reset {
            object_token: OBJECT_TOKEN,
            source,
            lane_ordinal: 2,
            observed_at: 45,
            phase: ScopedAppendDeliveryPhase::Correction,
            reset: ScopedAppendReset {
                old_generation: 1,
                new_generation: 2,
                reason: AppendTransition::Truncated,
            },
        })
        .unwrap();
    assert_eq!(projected.len(), 1);
    assert_eq!(sink.artifact_evidence_snapshot().unwrap().entry_count, 0);
}
