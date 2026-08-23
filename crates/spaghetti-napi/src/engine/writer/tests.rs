use super::*;
use crate::adapter::{
    AdapterId, ArtifactCapture, ArtifactContentFact, ArtifactMetadataEntry,
    ArtifactMetadataSnapshotFact, ArtifactObservationKind, EntityKey, Fact, QualifiedTimestamp,
    RawRetentionPolicy, TimestampQuality,
};
use crate::source::{RecordOrigin, SourceCursor, SourceMediaType, SourceRecord};
use rusqlite::OptionalExtension;
use tempfile::tempdir;

fn grouped_request(index: u8) -> ObservationCommit {
    ObservationCommit {
        source: commit::SourceInstanceSpec {
            adapter_id: "writer-group-fixture".to_string(),
            stable_key: vec![index],
            display_name: format!("fixture-{index}"),
            adapter_version: "1.0.0".to_string(),
            adapter_contract_version: 1,
            source_schema_versions: Vec::new(),
            capabilities: Vec::new(),
            discovered_at: 1,
            last_seen_at: 1,
        },
        stream: commit::SourceStreamSpec {
            stream_key: "records".to_string(),
            driver_kind: "replace_document".to_string(),
            decoder_key: "fixture".to_string(),
            stream_state: "available".to_string(),
            last_reconciled_at: Some(1),
            consistency: crate::adapter::ConsistencyPolicy::SnapshotReplace,
            retention: RawRetentionPolicy::HashOnly,
        },
        object: commit::SourceObjectUpdate {
            object_key: vec![index],
            expected: commit::ExpectedSourceCursor::Absent,
            display_path: None,
            native_identity: None,
            generation: 1,
            committed_cursor: b"complete".to_vec(),
            observed_revision: None,
            adapter_object_context: None,
            driver_checkpoint: None,
            driver_checkpoint_version: None,
            decoder_state: None,
            decoder_state_version: None,
            retry_state: None,
            size_bytes: None,
            mtime_ns: None,
            decoder_contract_version: 1,
            state: "active".to_string(),
        },
        reason: "group-test".to_string(),
        started_at: 1,
        committed_at: 2,
        fact_count: 0,
        projection_versions: Vec::new(),
        record_errors: Vec::new(),
        changes: Vec::new(),
    }
}

fn commit_command(
    request: ObservationCommit,
) -> (WriterCommand, Receiver<Result<CommitReceipt, EngineError>>) {
    let (response, receive) = bounded(1);
    (
        WriterCommand::Commit {
            request: Box::new(request),
            queued_at: Instant::now(),
            response,
        },
        receive,
    )
}

fn fact_command(
    request: ObservationCommit,
    batch: FactBatch,
) -> (WriterCommand, Receiver<Result<CommitReceipt, EngineError>>) {
    let (response, receive) = bounded(1);
    (
        WriterCommand::CommitFacts {
            request: Box::new(request),
            batch: Box::new(batch),
            queued_at: Instant::now(),
            response,
        },
        receive,
    )
}

#[test]
fn writer_connection_stays_alive_until_shutdown() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("writer.db");
    let mut runtime = WriterRuntime::start(database).unwrap();
    let client = runtime.client();

    let health = client.health().unwrap();
    assert!(client.is_alive());
    assert_eq!(health.journal_mode, "wal");

    runtime.shutdown().unwrap();
    assert!(!client.is_alive());
    assert!(matches!(
        client.health(),
        Err(EngineError::WorkerUnavailable { worker: "writer" })
    ));
}

#[test]
fn projection_administration_counts_only_durable_writer_commits() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("projection-telemetry.db");
    let mut runtime = WriterRuntime::start(database.clone()).unwrap();
    let client = runtime.client();
    let source = client.commit_observation(grouped_request(1)).unwrap();
    let request = commit::ProjectionVersionCommit {
        source_instance_id: source.source_instance_id,
        reason: "projection.runtime.usage-v2.ready".to_string(),
        started_at: 3,
        committed_at: 4,
        projection_versions: vec![commit::ProjectionVersionUpdate {
            projection_id: "runtime.usage-v2".to_string(),
            scope_key: vec![1],
            desired_version: 1,
            completed_version: Some(1),
            readiness: commit::ProjectionReadiness::Ready,
            detail: None,
        }],
        coverage_sets: Vec::new(),
        coverage_preconditions: Vec::new(),
    };
    assert!(client
        .commit_projection_versions(request.clone())
        .unwrap()
        .is_some());
    assert!(client
        .commit_projection_versions(request)
        .unwrap()
        .is_none());

    let snapshot = client.performance_snapshot();
    assert_eq!(snapshot.commit_attempts, 2);
    assert_eq!(snapshot.committed, 2);
    assert_eq!(snapshot.failed, 0);
    runtime.shutdown().unwrap();
    assert_eq!(
        Connection::open(database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn disk_reserve_is_bounded_and_keeps_two_percent_on_normal_volumes() {
    assert_eq!(
        disk_reserve_bytes(10 * 1024 * 1024 * 1024),
        MIN_DISK_RESERVE_BYTES
    );
    assert_eq!(
        disk_reserve_bytes(100 * 1024 * 1024 * 1024),
        2 * 1024 * 1024 * 1024
    );
    assert_eq!(disk_reserve_bytes(u64::MAX), MAX_DISK_RESERVE_BYTES);
}

#[test]
fn bootstrap_collects_queued_commits_up_to_the_fact_bound() {
    let (tx, rx) = bounded(8);
    let (first, first_rx) = commit_command(grouped_request(1));
    let mut held = vec![first_rx];
    for index in 2..=5 {
        let (command, response) = commit_command(grouped_request(index));
        tx.send(command).unwrap();
        held.push(response);
    }
    let (group, leftover) = collect_commit_group(&rx, first, true);
    assert_eq!(group.len(), 5);
    assert!(leftover.is_none());
    drop(tx);
    drop(held);
}

#[test]
fn queued_logical_commits_share_one_physical_transaction() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("grouped-commit.db");
    let mut connection = open_writer(&database).unwrap();
    let telemetry = WriterTelemetry::new();
    let mut checkpoints = CheckpointController::new(&database);
    let mut commands = Vec::new();
    let mut responses = Vec::new();
    for index in 1..=4 {
        let (command, response) = commit_command(grouped_request(index));
        commands.push(command);
        responses.push(response);
    }

    process_commit_group(
        &mut connection,
        &database,
        &telemetry,
        &mut checkpoints,
        commands,
        true,
        false,
    );

    for response in responses {
        response.recv().unwrap().unwrap();
    }
    let snapshot = telemetry.snapshot(0);
    assert_eq!(snapshot.commit_attempts, 4);
    assert_eq!(snapshot.committed, 4);
    assert_eq!(snapshot.failed, 0);
    assert_eq!(
        snapshot
            .timings
            .iter()
            .find(|timing| timing.name == "physical_transaction")
            .unwrap()
            .latency
            .samples,
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn failed_group_rolls_back_before_isolated_retry() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("grouped-fallback.db");
    let mut connection = open_writer(&database).unwrap();
    let telemetry = WriterTelemetry::new();
    let mut checkpoints = CheckpointController::new(&database);
    let duplicate = grouped_request(1);
    let (first, first_response) = commit_command(duplicate.clone());
    let (second, second_response) = commit_command(duplicate);

    process_commit_group(
        &mut connection,
        &database,
        &telemetry,
        &mut checkpoints,
        vec![first, second],
        true,
        false,
    );

    first_response.recv().unwrap().unwrap();
    assert!(matches!(
        second_response.recv().unwrap(),
        Err(EngineError::StaleSourceCursor { .. })
    ));
    let snapshot = telemetry.snapshot(0);
    assert_eq!(snapshot.commit_attempts, 2);
    assert_eq!(snapshot.committed, 1);
    assert_eq!(snapshot.failed, 1);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn bootstrap_commits_omit_public_change_log_rows() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("bootstrap-changelog.db");
    let mut connection = open_writer(&database).unwrap();
    let telemetry = WriterTelemetry::new();
    let mut checkpoints = CheckpointController::new(&database);
    let mut request = grouped_request(1);
    request.changes.push(commit::ChangeEntry {
        topic: "history.message.changed".to_string(),
        schema_version: 1,
        entity_key: b"message".to_vec(),
        operation: "upsert".to_string(),
        payload: Vec::new(),
    });
    let (command, response) = commit_command(request);

    process_commit_group(
        &mut connection,
        &database,
        &telemetry,
        &mut checkpoints,
        vec![command],
        true,
        true,
    );
    response.recv().unwrap().unwrap();

    let change_log_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM change_log", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        change_log_rows, 0,
        "bootstrap ingest publishes a snapshot watermark instead of historical change-log rows"
    );
    let commits: i64 = connection
        .query_row("SELECT COUNT(*) FROM ingest_commits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(commits, 1);
}

#[test]
fn bootstrap_finalization_rebuilds_deferred_artifact_state_before_readiness() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("bootstrap-artifact.db");
    let mut connection = open_writer(&database).unwrap();
    assert!(schema::begin_query_bootstrap(&mut connection).unwrap());
    schema::set_bootstrap_ingest_pragmas(&connection).unwrap();

    let mut request = grouped_request(1);
    request.object.committed_cursor = SourceCursor::append_offset(1).into_bytes();
    let source_instance_id =
        commit::source_instance_catalog_id(&request.source.adapter_id, &request.source.stable_key);
    let source_stream_id =
        commit::source_stream_catalog_id(source_instance_id, &request.stream.stream_key);
    let source_object_id =
        commit::source_object_catalog_id(source_stream_id, &request.object.object_key);
    let record = SourceRecord::new(
        &RecordOrigin {
            source_instance_id,
            stream_id: source_stream_id,
            object_id: source_object_id,
            observed_at: 1,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        },
        1,
        SourceCursor::append_offset(0),
        SourceCursor::append_offset(1),
        0,
        b"bootstrap-artifact".to_vec(),
    );
    let adapter = AdapterId::new("writer-group-fixture").unwrap();
    let artifact = EntityKey::native(&adapter, 1, "artifact", b"artifact-1").unwrap();
    let session = EntityKey::native(&adapter, 1, "session", b"session-1").unwrap();
    let source_time = QualifiedTimestamp {
        value: "2026-08-15T00:00:00.000Z".to_string(),
        quality: TimestampQuality::NativeExact,
    };
    let native_artifact_id = "artifact-hash@v1";
    let content = b"bootstrap artifact content\n".to_vec();
    let mut batch = FactBatch::new(2, 1).unwrap();
    batch
        .push(
            &record,
            Fact::ArtifactMetadataSnapshot(ArtifactMetadataSnapshotFact {
                session: session.clone(),
                canonical_session: None,
                native_message_id: "message-1".to_string(),
                native_snapshot_message_id: "snapshot-1".to_string(),
                observation_kind: ArtifactObservationKind::Delta,
                is_snapshot_update: false,
                source_time: Some(source_time.clone()),
                artifacts: vec![ArtifactMetadataEntry {
                    artifact: artifact.clone(),
                    canonical_artifact: None,
                    native_artifact_id: Some(native_artifact_id.to_string()),
                    tracking_path: "src/bootstrap.rs".to_string(),
                    real_parent_dir: Some("/fixture".to_string()),
                    version: 1,
                    backup_time: source_time,
                    capture: ArtifactCapture::ContentExpected,
                }],
            }),
        )
        .unwrap();
    batch
        .push(
            &record,
            Fact::ArtifactContent(ArtifactContentFact {
                artifact: artifact.clone(),
                session,
                canonical_artifact: None,
                canonical_session: None,
                native_artifact_id: native_artifact_id.to_string(),
                native_file_hash: "artifact-hash".to_string(),
                version: 1,
                size_bytes: content.len() as u64,
                content,
            }),
        )
        .unwrap();

    let telemetry = WriterTelemetry::new();
    let mut checkpoints = CheckpointController::new(&database);
    let (command, response) = fact_command(request, batch);
    process_commit_group(
        &mut connection,
        &database,
        &telemetry,
        &mut checkpoints,
        vec![command],
        true,
        true,
    );
    response.recv().unwrap().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM canonical_artifacts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert!(query_bootstrap_active(&connection).unwrap());

    assert_eq!(
        finalize_query_bootstrap_connection_profiled(&mut connection, &telemetry).unwrap(),
        Some(1)
    );
    let final_state: (String, String, i64, i64) = connection
            .query_row(
                "SELECT resolution_status, content_status, metadata_assertion_count, content_assertion_count FROM canonical_artifacts WHERE artifact_key = ?1",
                [artifact.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(
        final_state,
        ("resolved".to_string(), "captured".to_string(), 1, 1)
    );
    assert!(!query_bootstrap_active(&connection).unwrap());
    let phases = telemetry
        .snapshot(0)
        .timings
        .into_iter()
        .map(|timing| timing.name)
        .collect::<Vec<_>>();
    assert!(phases
        .iter()
        .any(|phase| phase == "bootstrap.artifact_rebuild"));
    assert!(phases
        .iter()
        .any(|phase| phase == "bootstrap.foreign_key_check"));
    assert!(phases
        .iter()
        .any(|phase| phase == "bootstrap.fts_integrity_check"));
    assert!(!phases.iter().any(|phase| phase == "bootstrap.quick_check"));
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(1_i64))
            .optional()
            .unwrap(),
        None
    );
}

#[test]
fn recovered_bootstrap_retains_structural_integrity_scan() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("recovered-bootstrap.db");
    let mut connection = open_writer(&database).unwrap();
    assert!(schema::begin_query_bootstrap(&mut connection).unwrap());
    let mut phases = Vec::new();
    assert_eq!(
        finalize_query_bootstrap_connection_observed(&mut connection, true, |phase, _| {
            phases.push(phase.to_string());
        })
        .unwrap(),
        Some(0)
    );
    assert!(phases.iter().any(|phase| phase == "quick_check"));
    assert!(phases.iter().any(|phase| phase == "foreign_key_check"));
    assert!(phases.iter().any(|phase| phase == "fts_integrity_check"));
}

#[test]
fn bootstrap_defers_checkpoints_until_the_large_wal_threshold() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("bootstrap-checkpoint.db");
    let connection = open_writer(&database).unwrap();
    let telemetry = WriterTelemetry::new();
    let mut checkpoints = CheckpointController::new(&database);
    checkpoints.maybe_checkpoint(&connection, &telemetry, true);
    assert_eq!(telemetry.snapshot(0).checkpoint.attempts, 0);
    checkpoints.maybe_checkpoint(&connection, &telemetry, false);
    assert_eq!(
        telemetry.snapshot(0).checkpoint.attempts,
        0,
        "an empty WAL stays below both live and bootstrap thresholds"
    );
}

#[test]
fn reader_free_bootstrap_checkpoint_copies_and_reclaims_the_wal() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("reader-free-checkpoint.db");
    let connection = open_writer(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE checkpoint_fixture(value BLOB NOT NULL); \
                 BEGIN IMMEDIATE; \
                 INSERT INTO checkpoint_fixture(value) VALUES (zeroblob(1048576)); \
                 COMMIT;",
        )
        .unwrap();

    let mut wal_path = database.as_os_str().to_os_string();
    wal_path.push("-wal");
    let wal_path = PathBuf::from(wal_path);
    assert!(
        std::fs::metadata(&wal_path)
            .map(|metadata| metadata.len())
            .unwrap_or_default()
            > 0,
        "fixture must create WAL frames before the checkpoint"
    );

    let checkpoint = reader_free_checkpoint(&connection).unwrap();
    assert!(!checkpoint.busy, "{checkpoint:?}");
    assert_eq!(checkpoint.remaining_frames, 0);
    assert_eq!(
        std::fs::metadata(wal_path)
            .map(|metadata| metadata.len())
            .unwrap_or_default(),
        0,
        "reader-free TRUNCATE must reclaim the WAL"
    );
}

#[cfg(unix)]
#[test]
fn writer_database_is_restricted_to_the_current_user() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("permissions.db");
    let mut runtime = WriterRuntime::start(database.clone()).unwrap();
    assert_eq!(
        std::fs::metadata(&database).unwrap().permissions().mode() & 0o777,
        0o600
    );
    runtime.shutdown().unwrap();
}

// Test-only blocking wrapper over `submit_observation`; the writer worker
// itself never waits on its own queue.
impl WriterClient {
    pub fn commit_observation(
        &self,
        request: ObservationCommit,
    ) -> Result<CommitReceipt, EngineError> {
        self.submit_observation(request)?
            .recv()
            .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
    }
}
