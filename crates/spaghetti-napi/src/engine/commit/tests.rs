use super::*;
use crate::adapter::{
    CanonicalSourceInstanceKey, CoverageDeclarationDigest, CoverageDomain,
    CoverageMembershipRevision, CoverageScope, CoverageSetCompleteness, SourceCoverageSet,
};
use crate::core::schema;
use crate::engine::{ChangeReplayRequest, EngineOptions, SpaghettiEngineCore};
use tempfile::tempdir;

struct FailAt(CommitStage);

impl CommitHook for FailAt {
    fn reach(&self, stage: CommitStage) -> Result<(), EngineError> {
        if stage == self.0 {
            Err(EngineError::InjectedFailure {
                stage: stage_name(stage),
            })
        } else {
            Ok(())
        }
    }
}

struct FailProjectionAt(ProjectionCommitStage);

impl ProjectionCommitHook for FailProjectionAt {
    fn reach(&self, stage: ProjectionCommitStage) -> Result<(), EngineError> {
        if stage == self.0 {
            Err(EngineError::InjectedFailure {
                stage: projection_stage_name(stage),
            })
        } else {
            Ok(())
        }
    }
}

struct FixtureProjectionWork;

impl TransactionalProjectionWork for FixtureProjectionWork {
    fn apply_canonical(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        write_fixture_projection(transaction, "fixture_canonical", context)?;
        Ok(Vec::new())
    }

    fn apply_runtime(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        write_fixture_projection(transaction, "fixture_runtime", context)?;
        Ok(Vec::new())
    }

    fn apply_usage(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        write_fixture_projection(transaction, "fixture_usage", context)?;
        Ok(Vec::new())
    }
}

fn write_fixture_projection(
    transaction: &Transaction<'_>,
    table: &'static str,
    context: &ProjectionCommitContext,
) -> Result<(), EngineError> {
    let sql = match table {
        "fixture_canonical" => "INSERT INTO fixture_canonical VALUES (?1, ?2, ?3, ?4, ?5)",
        "fixture_runtime" => "INSERT INTO fixture_runtime VALUES (?1, ?2, ?3, ?4, ?5)",
        "fixture_usage" => "INSERT INTO fixture_usage VALUES (?1, ?2, ?3, ?4, ?5)",
        _ => unreachable!(),
    };
    transaction
        .execute(
            sql,
            params![
                to_i64(context.commit_seq, "fixture commit sequence")?,
                to_i64(context.source_instance_id, "fixture source instance")?,
                to_i64(context.source_stream_id, "fixture source stream")?,
                to_i64(context.source_object_id, "fixture source object")?,
                to_i64(context.generation, "fixture generation")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("write fixture projection", error))
}

fn stage_name(stage: CommitStage) -> &'static str {
    match stage {
        CommitStage::BeforeTransaction => "before transaction",
        CommitStage::MidCanonicalProjection => "mid canonical projection",
        CommitStage::MidRuntimeProjection => "mid runtime projection",
        CommitStage::MidUsageProjection => "mid usage projection",
        CommitStage::AfterCursorUpdate => "after cursor update",
        CommitStage::AfterOutboxInsert => "after outbox insert",
        CommitStage::BeforeCommit => "before commit",
        CommitStage::AfterCommit => "after commit",
        CommitStage::BeforePublish => "before in-memory publish",
    }
}

fn projection_stage_name(stage: ProjectionCommitStage) -> &'static str {
    match stage {
        ProjectionCommitStage::BeforeTransaction => "before projection transaction",
        ProjectionCommitStage::AfterCommitInsert => "after projection commit insert",
        ProjectionCommitStage::AfterProjectionVersions => "after projection version writes",
        ProjectionCommitStage::AfterCoverageReplacement => "after projection coverage replacement",
        ProjectionCommitStage::BeforeCommit => "before projection commit",
        ProjectionCommitStage::AfterCommit => "after projection commit",
    }
}

fn initialize_fixture_tables(connection: &Connection) {
    connection
        .execute_batch(
            r#"
                CREATE TABLE fixture_canonical(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_runtime(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_usage(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                "#,
        )
        .unwrap();
}

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&connection).unwrap();
    initialize_fixture_tables(&connection);
    connection
}

fn request() -> ObservationCommit {
    ObservationCommit {
        source: SourceInstanceSpec {
            adapter_id: "fixture".to_string(),
            stable_key: b"fixture-root".to_vec(),
            display_name: "Fixture root".to_string(),
            adapter_version: "1.0.0".to_string(),
            adapter_contract_version: 3,
            source_schema_versions: Vec::new(),
            capabilities: Vec::new(),
            discovered_at: 1_000,
            last_seen_at: 1_100,
        },
        stream: SourceStreamSpec {
            stream_key: "transcripts".to_string(),
            driver_kind: "append_file".to_string(),
            decoder_key: "fixture.jsonl".to_string(),
            stream_state: "available".to_string(),
            last_reconciled_at: Some(1_050),
            consistency: ConsistencyPolicy::IncrementalCursor,
            retention: RawRetentionPolicy::Full,
        },
        object: SourceObjectUpdate {
            object_key: b"session-1".to_vec(),
            expected: ExpectedSourceCursor::Absent,
            display_path: Some("sessions/1.jsonl".to_string()),
            native_identity: Some(b"inode:1".to_vec()),
            generation: 1,
            committed_cursor: b"byte:128".to_vec(),
            observed_revision: Some(b"rev:1".to_vec()),
            adapter_object_context: Some(b"context".to_vec()),
            driver_checkpoint: Some(b"append-checkpoint".to_vec()),
            driver_checkpoint_version: Some(1),
            decoder_state: Some(b"decoder".to_vec()),
            decoder_state_version: Some(2),
            retry_state: None,
            size_bytes: Some(128),
            mtime_ns: Some(1_000_000),
            decoder_contract_version: 4,
            state: "active".to_string(),
        },
        reason: "live_append".to_string(),
        started_at: 1_100,
        committed_at: 1_200,
        fact_count: 2,
        projection_versions: vec![ProjectionVersionUpdate {
            projection_id: "canonical.history".to_string(),
            scope_key: b"fixture-root".to_vec(),
            desired_version: 5,
            completed_version: Some(5),
            readiness: ProjectionReadiness::Ready,
            detail: None,
        }],
        record_errors: vec![SourceRecordError {
            generation: 1,
            cursor_start: b"byte:64".to_vec(),
            cursor_end: b"byte:80".to_vec(),
            payload_hash: b"sha256:fixture".to_vec(),
            media_type: "application/x-ndjson".to_string(),
            raw_payload: None,
            error_class: "unknown_record".to_string(),
            error_message: "future fixture record".to_string(),
            adapter_version: "1.0.0".to_string(),
            contract_version: 3,
            last_retry_at: None,
        }],
        changes: vec![
            ChangeEntry {
                topic: "history.session.changed".to_string(),
                schema_version: 1,
                entity_key: b"session-1".to_vec(),
                operation: "upsert".to_string(),
                payload: br#"{"session":"session-1"}"#.to_vec(),
            },
            ChangeEntry {
                topic: "runtime.session.changed".to_string(),
                schema_version: 1,
                entity_key: b"session-1".to_vec(),
                operation: "upsert".to_string(),
                payload: br#"{"state":"active"}"#.to_vec(),
            },
        ],
    }
}

fn usage_v2_projection_commit(source_instance_id: u64) -> ProjectionVersionCommit {
    let domain = CoverageDomain::FactFamily {
        family: "runtime.usage-v2".to_string(),
        version: 1,
    };
    let source_instance_key = CanonicalSourceInstanceKey::derive(1, b"fixture-root").unwrap();
    let coverage = SourceCoverageSet::new(
        domain,
        CoverageScope {
            adapter_id: "fixture".to_string(),
            source_instance_key,
            root_entity_key: None,
            support_release_id: "fixture-support-release".to_string(),
            source_or_scope_declaration_digest: CoverageDeclarationDigest::derive(
                b"fixture-source-declaration",
            )
            .unwrap(),
        },
        CoverageMembershipRevision::derive(b"fixture-empty-membership").unwrap(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        CoverageSetCompleteness::Complete,
    )
    .unwrap();
    ProjectionVersionCommit {
        source_instance_id,
        reason: "projection.runtime.usage-v2.ready".to_string(),
        started_at: 1_300,
        committed_at: 1_301,
        projection_versions: vec![ProjectionVersionUpdate {
            projection_id: "runtime.usage-v2".to_string(),
            scope_key: b"fixture-root".to_vec(),
            desired_version: 1,
            completed_version: Some(1),
            readiness: ProjectionReadiness::Ready,
            detail: None,
        }],
        coverage_sets: vec![DurableCoverageSetUpdate {
            owner_id: "runtime.usage-v2".to_string(),
            owner_scope_key: b"fixture-root".to_vec(),
            set: coverage,
        }],
        coverage_preconditions: Vec::new(),
    }
}

fn count(connection: &Connection, table: &'static str) -> i64 {
    let sql = match table {
        "source_instances" => "SELECT COUNT(*) FROM source_instances",
        "source_objects" => "SELECT COUNT(*) FROM source_objects",
        "ingest_commits" => "SELECT COUNT(*) FROM ingest_commits",
        "projection_versions" => "SELECT COUNT(*) FROM projection_versions",
        "source_coverage_sets" => "SELECT COUNT(*) FROM source_coverage_sets",
        "source_record_errors" => "SELECT COUNT(*) FROM source_record_errors",
        "change_log" => "SELECT COUNT(*) FROM change_log",
        "fixture_canonical" => "SELECT COUNT(*) FROM fixture_canonical",
        "fixture_runtime" => "SELECT COUNT(*) FROM fixture_runtime",
        "fixture_usage" => "SELECT COUNT(*) FROM fixture_usage",
        _ => unreachable!(),
    };
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn commit_atomically_persists_catalog_cursor_projection_diagnostics_and_outbox() {
    let mut connection = database();
    let receipt = apply_observation_commit_with_components(
        &mut connection,
        &request(),
        &FixtureProjectionWork,
        &NoopCommitHook,
    )
    .unwrap();

    assert_eq!(receipt.commit_seq, 1);
    assert_eq!(receipt.change_count, 2);
    assert_eq!(count(&connection, "source_instances"), 1);
    assert_eq!(count(&connection, "source_objects"), 1);
    assert_eq!(count(&connection, "ingest_commits"), 1);
    assert_eq!(count(&connection, "projection_versions"), 1);
    assert_eq!(count(&connection, "source_record_errors"), 1);
    assert_eq!(count(&connection, "change_log"), 2);
    assert_eq!(count(&connection, "fixture_canonical"), 1);
    assert_eq!(count(&connection, "fixture_runtime"), 1);
    assert_eq!(count(&connection, "fixture_usage"), 1);
    let raw_retention: String = connection
        .query_row("SELECT raw_retention FROM source_streams", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(raw_retention, "full");
    let provenance: (i64, i64, i64, i64, i64) = connection
        .query_row(
            r#"
                SELECT commit_seq, source_instance_id, source_stream_id,
                       source_object_id, generation
                FROM fixture_canonical
                "#,
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        provenance,
        (
            1,
            to_i64(receipt.source_instance_id, "test source instance").unwrap(),
            to_i64(receipt.source_stream_id, "test source stream").unwrap(),
            to_i64(receipt.source_object_id, "test source object").unwrap(),
            1,
        )
    );

    let commit: (i64, i64) = connection
        .query_row(
            "SELECT committed_at, fact_count FROM ingest_commits WHERE commit_seq = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(commit, (1_200, 2));
    let cursor: (i64, Vec<u8>, i64) = connection
        .query_row(
            "SELECT generation, committed_cursor, last_commit_seq FROM source_objects",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(cursor, (1, b"byte:128".to_vec(), 1));
    let opaque_state: (Vec<u8>, i64, Vec<u8>, i64) = connection
        .query_row(
            r#"
                SELECT driver_checkpoint, driver_checkpoint_version,
                       decoder_state, decoder_state_version
                FROM source_objects
                "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        opaque_state,
        (b"append-checkpoint".to_vec(), 1, b"decoder".to_vec(), 2)
    );
    let projection: (i64, String, i64) = connection
        .query_row(
            "SELECT completed_version, readiness, last_commit_seq FROM projection_versions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(projection, (5, "ready".to_string(), 1));
}

#[test]
fn projection_readiness_advances_the_commit_clock_without_source_cursor_churn() {
    let mut connection = database();
    let source = apply_observation_commit(&mut connection, &request()).unwrap();
    let pending = ProjectionVersionUpdate {
        projection_id: "runtime.usage-v2".to_string(),
        scope_key: b"fixture-root".to_vec(),
        desired_version: 1,
        completed_version: None,
        readiness: ProjectionReadiness::Pending,
        detail: Some("source reconciliation in progress".to_string()),
    };
    let pending_request = ProjectionVersionCommit {
        source_instance_id: source.source_instance_id,
        reason: "projection.runtime.usage-v2.pending".to_string(),
        started_at: 1_300,
        committed_at: 1_301,
        projection_versions: vec![pending.clone()],
        coverage_sets: Vec::new(),
        coverage_preconditions: Vec::new(),
    };
    let receipt = apply_projection_version_commit(&mut connection, &pending_request)
        .unwrap()
        .expect("first transition advances the commit clock");
    assert_eq!(receipt.commit_seq, 2);
    assert_eq!(count(&connection, "ingest_commits"), 2);
    assert_eq!(count(&connection, "source_objects"), 1);
    assert_eq!(count(&connection, "change_log"), 2);
    assert!(
        apply_projection_version_commit(&mut connection, &pending_request)
            .unwrap()
            .is_none(),
        "an equal transition must not churn the durable watermark"
    );
    assert_eq!(count(&connection, "ingest_commits"), 2);

    let ready_request = ProjectionVersionCommit {
        source_instance_id: source.source_instance_id,
        reason: "projection.runtime.usage-v2.ready".to_string(),
        started_at: 1_400,
        committed_at: 1_401,
        projection_versions: vec![ProjectionVersionUpdate {
            completed_version: Some(1),
            readiness: ProjectionReadiness::Ready,
            detail: None,
            ..pending
        }],
        coverage_sets: Vec::new(),
        coverage_preconditions: Vec::new(),
    };
    let ready = apply_projection_version_commit(&mut connection, &ready_request)
        .unwrap()
        .expect("readiness transition advances the commit clock");
    assert_eq!(ready.commit_seq, 3);
    assert_eq!(
            connection
                .query_row(
                    "SELECT desired_version, completed_version, readiness, last_commit_seq, detail FROM projection_versions WHERE projection_id = 'runtime.usage-v2'",
                    [],
                    |row| Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    )),
                )
                .unwrap(),
            (1, 1, "ready".to_string(), 3, None)
        );
}

#[test]
fn projection_and_coverage_precommit_failure_seams_roll_back_together() {
    let stages = [
        ProjectionCommitStage::BeforeTransaction,
        ProjectionCommitStage::AfterCommitInsert,
        ProjectionCommitStage::AfterProjectionVersions,
        ProjectionCommitStage::AfterCoverageReplacement,
        ProjectionCommitStage::BeforeCommit,
    ];

    for stage in stages {
        let mut connection = database();
        let source = apply_observation_commit(&mut connection, &request()).unwrap();
        let migration = usage_v2_projection_commit(source.source_instance_id);
        let result = apply_projection_version_commit_with_hook(
            &mut connection,
            &migration,
            &FailProjectionAt(stage),
        );
        assert!(
            matches!(result, Err(EngineError::InjectedFailure { .. })),
            "stage {stage:?} returned {result:?}"
        );
        assert_eq!(count(&connection, "ingest_commits"), 1, "{stage:?}");
        assert_eq!(
            count(&connection, "source_coverage_sets"),
            0,
            "{stage:?} leaked coverage"
        );
        assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM projection_versions WHERE projection_id = 'runtime.usage-v2'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "{stage:?} leaked readiness"
            );

        let receipt = apply_projection_version_commit(&mut connection, &migration)
            .unwrap()
            .expect("retry after a precommit crash must apply once");
        assert_eq!(receipt.commit_seq, 2, "{stage:?}");
        assert_eq!(count(&connection, "ingest_commits"), 2, "{stage:?}");
        assert_eq!(count(&connection, "source_coverage_sets"), 1, "{stage:?}");
        assert_eq!(
            connection
                .query_row(
                    r#"
                        SELECT projection.readiness,
                               projection.last_commit_seq,
                               coverage.completeness,
                               coverage.last_commit_seq
                        FROM projection_versions AS projection
                        JOIN source_coverage_sets AS coverage
                          ON coverage.owner_id = projection.projection_id
                         AND coverage.owner_scope_key = projection.scope_key
                        WHERE projection.projection_id = 'runtime.usage-v2'
                        "#,
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .unwrap(),
            ("ready".to_string(), 2, "complete".to_string(), 2),
            "{stage:?} did not converge atomically"
        );
    }
}

#[test]
fn projection_postcommit_ack_loss_is_restart_idempotent() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("projection-postcommit-restart.db");
    let source_instance_id;

    {
        let mut connection = Connection::open(&database_path).unwrap();
        schema::initialize_schema(&connection).unwrap();
        initialize_fixture_tables(&connection);
        source_instance_id = apply_observation_commit(&mut connection, &request())
            .unwrap()
            .source_instance_id;
        let migration = usage_v2_projection_commit(source_instance_id);
        let result = apply_projection_version_commit_with_hook(
            &mut connection,
            &migration,
            &FailProjectionAt(ProjectionCommitStage::AfterCommit),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(count(&connection, "ingest_commits"), 2);
        assert_eq!(count(&connection, "source_coverage_sets"), 1);
    }

    let mut connection = Connection::open(&database_path).unwrap();
    schema::initialize_schema(&connection).unwrap();
    let migration = usage_v2_projection_commit(source_instance_id);
    assert!(
        apply_projection_version_commit(&mut connection, &migration)
            .unwrap()
            .is_none(),
        "retry after restart must recognize the durable transition"
    );
    assert_eq!(count(&connection, "ingest_commits"), 2);
    assert_eq!(count(&connection, "source_coverage_sets"), 1);
    assert_eq!(
        connection
            .query_row(
                r#"
                    SELECT projection.last_commit_seq, coverage.last_commit_seq
                    FROM projection_versions AS projection
                    JOIN source_coverage_sets AS coverage
                      ON coverage.owner_id = projection.projection_id
                     AND coverage.owner_scope_key = projection.scope_key
                    WHERE projection.projection_id = 'runtime.usage-v2'
                    "#,
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
        (2, 2)
    );
}

#[test]
fn cursor_compare_and_swap_makes_committed_range_retry_idempotent() {
    let mut connection = database();
    let original = request();
    let first = apply_observation_commit(&mut connection, &original).unwrap();

    assert!(matches!(
        apply_observation_commit(&mut connection, &original),
        Err(EngineError::StaleSourceCursor { .. })
    ));
    assert_eq!(count(&connection, "ingest_commits"), 1);
    assert_eq!(count(&connection, "change_log"), 2);

    let mut next = original;
    next.object.expected = ExpectedSourceCursor::At {
        generation: 1,
        committed_cursor: b"byte:128".to_vec(),
    };
    next.object.committed_cursor = b"byte:256".to_vec();
    next.started_at = 1_300;
    next.committed_at = 1_400;
    let receipt = apply_observation_commit(&mut connection, &next).unwrap();
    assert_eq!(receipt.commit_seq, 2);
    assert_eq!(receipt.source_instance_id, first.source_instance_id);
    assert_eq!(receipt.source_stream_id, first.source_stream_id);
    assert_eq!(receipt.source_object_id, first.source_object_id);
    assert_eq!(count(&connection, "ingest_commits"), 2);
}

#[test]
fn unchanged_source_registration_does_not_rewrite_the_hot_catalog_row() {
    let mut connection = database();
    let first = request();
    apply_observation_commit(&mut connection, &first).unwrap();
    connection
        .execute_batch(
            r#"
                CREATE TEMP TABLE source_instance_updates (marker INTEGER);
                CREATE TEMP TRIGGER observe_source_instance_update
                AFTER UPDATE ON source_instances BEGIN
                  INSERT INTO source_instance_updates VALUES (1);
                END;
                "#,
        )
        .unwrap();

    let mut second = first.clone();
    second.object.expected = ExpectedSourceCursor::At {
        generation: 1,
        committed_cursor: b"byte:128".to_vec(),
    };
    second.object.committed_cursor = b"byte:256".to_vec();
    second.started_at += 100;
    second.committed_at += 100;
    apply_observation_commit(&mut connection, &second).unwrap();
    let update_count = |connection: &Connection| {
        connection
            .query_row("SELECT COUNT(*) FROM source_instance_updates", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(update_count(&connection), 0);

    second.object.expected = ExpectedSourceCursor::At {
        generation: 1,
        committed_cursor: b"byte:256".to_vec(),
    };
    second.object.committed_cursor = b"byte:384".to_vec();
    second.source.last_seen_at += 1;
    second.started_at += 100;
    second.committed_at += 100;
    apply_observation_commit(&mut connection, &second).unwrap();
    assert_eq!(update_count(&connection), 1);
}

#[test]
fn automatic_retention_runs_on_a_bounded_commit_cadence() {
    let mut connection = database();
    let mut next = request();

    let first_maintenance_at =
        2_000 + i64::try_from(AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS).unwrap();
    for commit_seq in 1..=3 * AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
        let cursor = format!("byte:{}", commit_seq * 128).into_bytes();
        if commit_seq > 1 {
            next.object.expected = ExpectedSourceCursor::At {
                generation: 1,
                committed_cursor: format!("byte:{}", (commit_seq - 1) * 128).into_bytes(),
            };
        }
        next.object.committed_cursor = cursor;
        next.started_at = 1_000 + i64::try_from(commit_seq).unwrap();
        next.committed_at = if commit_seq <= 2 * AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
            2_000 + i64::try_from(commit_seq).unwrap()
        } else {
            first_maintenance_at
                + AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_MS
                + i64::try_from(commit_seq).unwrap()
        };
        apply_observation_commit(&mut connection, &next).unwrap();

        let last_pruned_at: Option<i64> = connection
            .query_row(
                "SELECT last_pruned_at FROM change_log_retention_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if commit_seq < AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
            assert_eq!(last_pruned_at, None);
        } else if commit_seq < 3 * AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
            assert_eq!(last_pruned_at, Some(first_maintenance_at));
        } else {
            assert_eq!(last_pruned_at, Some(next.committed_at));
        }
    }
}

#[test]
fn catalog_identity_is_independent_of_unrelated_registration_order() {
    fn commit_named(connection: &mut Connection, stable_key: &[u8]) -> CommitReceipt {
        let mut request = request();
        request.source.stable_key = stable_key.to_vec();
        request.source.display_name = String::from_utf8_lossy(stable_key).into_owned();
        request.object.object_key = b"shared-object-name".to_vec();
        apply_observation_commit(connection, &request).unwrap()
    }

    let mut first_order = database();
    let alpha_first = commit_named(&mut first_order, b"alpha-root");
    let beta_second = commit_named(&mut first_order, b"beta-root");

    let mut reverse_order = database();
    let beta_first = commit_named(&mut reverse_order, b"beta-root");
    let alpha_second = commit_named(&mut reverse_order, b"alpha-root");

    assert_eq!(
        alpha_first.source_instance_id,
        alpha_second.source_instance_id
    );
    assert_eq!(alpha_first.source_stream_id, alpha_second.source_stream_id);
    assert_eq!(alpha_first.source_object_id, alpha_second.source_object_id);
    assert_eq!(
        beta_second.source_instance_id,
        beta_first.source_instance_id
    );
    assert_eq!(beta_second.source_stream_id, beta_first.source_stream_id);
    assert_eq!(beta_second.source_object_id, beta_first.source_object_id);
    assert_ne!(
        alpha_first.source_instance_id,
        beta_first.source_instance_id
    );
}

#[test]
fn driver_checkpoint_requires_a_nonzero_paired_version_before_writes() {
    for (checkpoint, version) in [
        (Some(b"checkpoint".to_vec()), None),
        (None, Some(1)),
        (Some(b"checkpoint".to_vec()), Some(0)),
        (Some(vec![0; MAX_DRIVER_CHECKPOINT_BYTES + 1]), Some(1)),
    ] {
        let mut connection = database();
        let mut invalid = request();
        invalid.object.driver_checkpoint = checkpoint;
        invalid.object.driver_checkpoint_version = version;
        assert!(matches!(
            apply_observation_commit(&mut connection, &invalid),
            Err(EngineError::InvalidCommit(_))
        ));
        assert_eq!(count(&connection, "source_instances"), 0);
        assert_eq!(count(&connection, "source_objects"), 0);
        assert_eq!(count(&connection, "ingest_commits"), 0);
    }
}

#[test]
fn every_precommit_failure_seam_rolls_back_all_visible_effects() {
    let stages = [
        CommitStage::BeforeTransaction,
        CommitStage::MidCanonicalProjection,
        CommitStage::MidRuntimeProjection,
        CommitStage::MidUsageProjection,
        CommitStage::AfterCursorUpdate,
        CommitStage::AfterOutboxInsert,
        CommitStage::BeforeCommit,
    ];

    for stage in stages {
        let mut connection = database();
        let result = apply_observation_commit_with_components(
            &mut connection,
            &request(),
            &FixtureProjectionWork,
            &FailAt(stage),
        );
        assert!(
            matches!(result, Err(EngineError::InjectedFailure { .. })),
            "stage {stage:?} returned {result:?}"
        );
        for table in [
            "source_instances",
            "source_objects",
            "ingest_commits",
            "projection_versions",
            "source_record_errors",
            "change_log",
            "fixture_canonical",
            "fixture_runtime",
            "fixture_usage",
        ] {
            assert_eq!(count(&connection, table), 0, "{stage:?} leaked {table}");
        }
    }
}

#[test]
fn postcommit_failure_is_recoverable_and_retry_has_no_duplicate_effect() {
    for stage in [CommitStage::AfterCommit, CommitStage::BeforePublish] {
        let mut connection = database();
        let original = request();
        let result = apply_observation_commit_with_components(
            &mut connection,
            &original,
            &FixtureProjectionWork,
            &FailAt(stage),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(count(&connection, "ingest_commits"), 1);
        assert_eq!(count(&connection, "source_objects"), 1);
        assert_eq!(count(&connection, "projection_versions"), 1);
        assert_eq!(count(&connection, "change_log"), 2);
        assert_eq!(count(&connection, "fixture_canonical"), 1);
        assert_eq!(count(&connection, "fixture_runtime"), 1);
        assert_eq!(count(&connection, "fixture_usage"), 1);

        assert!(matches!(
            apply_observation_commit(&mut connection, &original),
            Err(EngineError::StaleSourceCursor { .. })
        ));
        assert_eq!(count(&connection, "ingest_commits"), 1);
        assert_eq!(count(&connection, "change_log"), 2);
    }
}

#[test]
fn restart_replays_an_outbox_committed_before_publication() {
    let directory = tempdir().unwrap();
    let database_path = directory.path().join("postcommit-replay.db");
    let mut connection = Connection::open(&database_path).unwrap();
    schema::set_pragmas(&connection).unwrap();
    schema::initialize_schema(&connection).unwrap();
    connection
        .execute_batch(
            r#"
                CREATE TABLE fixture_canonical(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_runtime(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_usage(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                "#,
        )
        .unwrap();

    let result = apply_observation_commit_with_components(
        &mut connection,
        &request(),
        &FixtureProjectionWork,
        &FailAt(CommitStage::BeforePublish),
    );
    assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
    drop(connection);

    let engine = SpaghettiEngineCore::open(EngineOptions {
        database_path,
        query_workers: Some(1),
        owner_label: Some("postcommit-restart-test".to_string()),
        defer_query_structures: false,
        source_pass_pool: None,
    })
    .unwrap();
    let replay = engine
        .replay_changes(ChangeReplayRequest {
            after: None,
            topics: Vec::new(),
            limit: 10,
        })
        .unwrap();
    assert_eq!(replay.at_commit_seq, 1);
    assert_eq!(replay.changes.len(), 2);
    assert_eq!(replay.changes[0].payload, br#"{"session":"session-1"}"#);
    assert_eq!(replay.changes[1].payload, br#"{"state":"active"}"#);
    engine.shutdown().unwrap();
}

// Test-only commit entry points. Production commits go through
// `apply_observation_commit_in_transaction` from `engine/writer.rs`; these
// wrappers own the transaction so a test can commit in one call.
struct NoopCommitHook;

/// Test-only convenience over [`apply_observation_commit_with_components`]:
/// the production writer commits inside its own transaction through
/// [`apply_observation_commit_in_transaction`].
pub(crate) fn apply_observation_commit(
    connection: &mut Connection,
    request: &ObservationCommit,
) -> Result<CommitReceipt, EngineError> {
    apply_observation_commit_with_components(
        connection,
        request,
        &NoProjectionWork,
        &NoopCommitHook,
    )
}

/// Test-only counterpart of [`apply_observation_commit`] for commits that
/// carry projection work.
pub(crate) fn apply_observation_commit_with_projection(
    connection: &mut Connection,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
) -> Result<CommitReceipt, EngineError> {
    apply_observation_commit_with_components(connection, request, projection_work, &NoopCommitHook)
}

pub(crate) fn apply_observation_commit_with_components(
    connection: &mut Connection,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
    hook: &dyn CommitHook,
) -> Result<CommitReceipt, EngineError> {
    prepare_observation_commit(request, hook)?;

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin ingest commit", error))?;
    let receipt = apply_observation_commit_components_in_transaction(
        &transaction,
        request,
        projection_work,
        hook,
        true,
        false,
    )?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit ingest transaction", error))?;
    complete_observation_commit(hook)?;
    Ok(receipt)
}

impl CommitHook for NoopCommitHook {
    fn reach(&self, _stage: CommitStage) -> Result<(), EngineError> {
        Ok(())
    }
}
