use super::*;
use rusqlite::Connection;

/// Count rows in `sqlite_master` matching a given type + name, used to
/// assert the presence of tables / triggers after operations.
fn object_exists(conn: &Connection, obj_type: &str, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
            [obj_type, name],
            |row| row.get(0),
        )
        .unwrap();
    count > 0
}

fn seed_bootstrap_message(conn: &Connection) {
    conn.execute_batch(
        r#"
        INSERT INTO source_instances (
            source_instance_id, adapter_id, stable_key, display_name,
            adapter_version, adapter_contract_version,
            source_schema_versions_json, capabilities_json,
            discovered_at, last_seen_at
        ) VALUES (1, 'fixture', x'01', 'fixture', '1', 1, '[]', '[]', 1, 1);
        INSERT INTO ingest_commits (
            commit_seq, source_instance_id, reason, started_at,
            committed_at, fact_count
        ) VALUES (1, 1, 'bootstrap', 1, 2, 1);
        INSERT INTO source_streams (
            source_stream_id, source_instance_id, stream_key, driver_kind,
            decoder_key, stream_state, last_commit_seq
        ) VALUES (1, 1, 'messages', 'append', 'fixture', 'active', 1);
        INSERT INTO source_objects (
            source_object_id, source_stream_id, object_key, generation,
            committed_cursor, decoder_contract_version, last_commit_seq,
            state
        ) VALUES (1, 1, x'01', 1, x'01', 1, 1, 'active');
        INSERT INTO fact_records (
            fact_id, fact_kind, entity_key, source_instance_id,
            source_stream_id, source_object_id, source_generation,
            cursor_start, cursor_end, payload_hash, local_fact_ordinal,
            observed_at, payload_json, last_commit_seq
        ) VALUES (
            x'01', 'message', x'01', 1, 1, 1, 1, x'00', x'01',
            zeroblob(32), 0, 1, x'', 1
        );
        INSERT INTO canonical_messages (
            message_key, session_key, run_key, native_message_id,
            native_kind, role, content_json, source_time,
            source_time_quality, search_text, raw_json, fact_id,
            source_object_id, source_generation, cursor_start, cursor_end,
            last_commit_seq
        ) VALUES (
            x'01', x'02', x'03', 'native-1', 'user', 'user', x'5B5D',
            '2026-08-13T00:00:00.000Z', 'native_exact',
            'bootstrap searchable marker', x'7B7D', x'01', 1, 1,
            x'00', x'01', 1
        );
        "#,
    )
    .expect("seed bootstrap content");
}

fn canonical_search_count(conn: &Connection, query: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM canonical_message_search_fts WHERE canonical_message_search_fts MATCH ?1",
        [query],
        |row| row.get(0),
    )
    .expect("query canonical FTS")
}

#[test]
fn durable_query_bootstrap_defers_only_reviewed_structures_and_rebuilds_fts() {
    let mut conn = Connection::open_in_memory().expect("open in-memory db");
    initialize_schema(&conn).expect("initialize schema");

    assert!(begin_query_bootstrap(&mut conn).expect("begin query bootstrap"));
    assert_eq!(
        query_bootstrap_state(&conn).unwrap().as_deref(),
        Some("building")
    );
    for (index, _) in BOOTSTRAP_QUERY_INDEXES {
        assert!(
            !object_exists(&conn, "index", index),
            "{index} remained active"
        );
    }
    assert!(object_exists(
        &conn,
        "index",
        "idx_fact_records_object_generation"
    ));
    assert!(object_exists(&conn, "index", "idx_run_evidence_decisive"));
    for trigger in CANONICAL_FTS_TRIGGERS {
        assert!(!object_exists(&conn, "trigger", trigger));
    }

    seed_bootstrap_message(&conn);
    assert_eq!(canonical_search_count(&conn, "bootstrap"), 0);
    assert_eq!(finalize_query_bootstrap(&mut conn).unwrap(), Some(1));
    assert_eq!(query_bootstrap_state(&conn).unwrap(), None);
    assert_eq!(canonical_search_count(&conn, "bootstrap"), 1);
    for (index, _) in BOOTSTRAP_QUERY_INDEXES {
        assert!(
            object_exists(&conn, "index", index),
            "{index} was not rebuilt"
        );
    }
    for trigger in CANONICAL_FTS_TRIGGERS {
        assert!(object_exists(&conn, "trigger", trigger));
    }
    let snapshot: String = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            [BOOTSTRAP_SNAPSHOT_COMMIT_KEY],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(snapshot, "1");
    assert!(!begin_query_bootstrap(&mut conn).unwrap());
}

#[test]
fn incomplete_query_bootstrap_recovers_before_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("bootstrap-recovery.sqlite");
    {
        let mut conn = Connection::open(&database).unwrap();
        set_pragmas(&conn).unwrap();
        initialize_schema(&conn).unwrap();
        assert!(begin_query_bootstrap(&mut conn).unwrap());
        seed_bootstrap_message(&conn);
        assert_eq!(canonical_search_count(&conn, "bootstrap"), 0);
    }

    let mut recovered = Connection::open(&database).unwrap();
    set_pragmas(&recovered).unwrap();
    initialize_schema(&recovered).unwrap();
    assert_eq!(
        query_bootstrap_state(&recovered).unwrap().as_deref(),
        Some("building")
    );
    assert!(recover_query_bootstrap(&mut recovered).unwrap());
    assert_eq!(query_bootstrap_state(&recovered).unwrap(), None);
    assert_eq!(canonical_search_count(&recovered, "bootstrap"), 1);
    assert!(!recover_query_bootstrap(&mut recovered).unwrap());
}

#[test]
fn deferred_foreign_keys_still_block_invalid_bootstrap_readiness() {
    let mut conn = Connection::open_in_memory().expect("open in-memory db");
    set_pragmas(&conn).expect("set writer pragmas");
    initialize_schema(&conn).expect("initialize schema");
    assert!(begin_query_bootstrap(&mut conn).expect("begin query bootstrap"));
    set_bootstrap_ingest_pragmas(&conn).expect("set bootstrap pragmas");
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );

    seed_bootstrap_message(&conn);
    conn.execute(
        "UPDATE canonical_messages SET fact_id = x'ff' WHERE message_key = x'01'",
        [],
    )
    .expect("inject orphaned message fact");
    let error = finalize_query_bootstrap(&mut conn).expect_err("orphan must block readiness");
    assert!(error.to_string().contains("foreign_key_check"));
    assert!(query_bootstrap_state(&conn).unwrap().is_some());

    conn.execute(
        "UPDATE canonical_messages SET fact_id = x'01' WHERE message_key = x'01'",
        [],
    )
    .expect("repair orphaned message fact");
    assert_eq!(finalize_query_bootstrap(&mut conn).unwrap(), Some(1));
    assert_eq!(query_bootstrap_state(&conn).unwrap(), None);
}

#[test]
fn initialize_schema_on_fresh_db_sets_version() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    initialize_schema(&conn).expect("initialize_schema");

    let version = current_schema_version(&conn).expect("read version");
    assert_eq!(version, Some(SCHEMA_VERSION));

    // Spot-check a handful of objects from every category.
    assert!(object_exists(&conn, "table", "schema_meta"));
    assert!(object_exists(&conn, "table", "projects"));
    assert!(object_exists(&conn, "table", "messages"));
    assert!(object_exists(&conn, "table", "timeline_tool_results"));
    assert!(object_exists(&conn, "table", "source_files"));
    assert!(object_exists(&conn, "table", "session_summary_totals"));
    assert!(object_exists(&conn, "table", "source_materializations"));
    assert!(object_exists(&conn, "table", "source_instances"));
    assert!(object_exists(&conn, "table", "source_streams"));
    assert!(object_exists(&conn, "table", "source_objects"));
    assert!(object_exists(&conn, "table", "change_log_retention_state"));
    let retention_state: (i64, i64, i64) = conn
        .query_row(
            r#"
            SELECT pruned_through_commit_seq, retained_change_count,
                   retained_payload_bytes
            FROM change_log_retention_state WHERE singleton = 1
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read initial retention state");
    assert_eq!(retention_state, (0, 0, 0));
    for column in [
        "driver_checkpoint",
        "driver_checkpoint_version",
        "retry_state",
    ] {
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('source_objects') WHERE name = ?1",
                [column],
                |row| row.get(0),
            )
            .expect("inspect source object schema");
        assert_eq!(present, 1, "missing source_objects.{column}");
    }
    assert!(object_exists(&conn, "table", "ingest_commits"));
    let source_commit_nullable: i64 = conn
        .query_row(
            "SELECT [notnull] FROM pragma_table_info('ingest_commits') WHERE name = 'source_instance_id'",
            [],
            |row| row.get(0),
        )
        .expect("inspect source-neutral commit ownership");
    assert_eq!(source_commit_nullable, 0);
    for table in [
        "catalog_sources",
        "catalog_projects",
        "catalog_sessions",
        "catalog_association_conflicts",
    ] {
        assert!(object_exists(&conn, "table", table), "missing {table}");
    }
    for column in [
        "external_ref",
        "association_basis",
        "association_quality",
        "association_provenance",
        "transcript_present",
        "source_modified_ms",
        "sort_time",
    ] {
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('catalog_sessions') WHERE name = ?1",
                [column],
                |row| row.get(0),
            )
            .expect("inspect catalog session columns");
        assert_eq!(present, 1, "missing catalog_sessions.{column}");
    }
    assert!(object_exists(&conn, "table", "projection_versions"));
    for table in [
        "source_coverage_sets",
        "source_coverage_points",
        "source_coverage_absences",
        "source_coverage_errors",
    ] {
        assert!(object_exists(&conn, "table", table), "missing {table}");
    }
    for index in [
        "idx_source_coverage_sets_instance_owner",
        "idx_source_coverage_points_object",
    ] {
        assert!(object_exists(&conn, "index", index), "missing {index}");
    }
    assert!(object_exists(&conn, "table", "source_record_errors"));
    assert!(object_exists(&conn, "table", "change_log"));
    assert!(object_exists(&conn, "table", "fact_records"));
    assert!(object_exists(&conn, "table", "unknown_native_evidence"));
    assert!(object_exists(
        &conn,
        "index",
        "idx_unknown_native_evidence_object_generation"
    ));
    assert!(object_exists(&conn, "table", "canonical_sessions"));
    assert!(object_exists(
        &conn,
        "table",
        "session_index_snapshot_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "session_index_entry_assertions"
    ));
    assert!(object_exists(&conn, "table", "canonical_session_indexes"));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_session_index_entries"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "project_memory_document_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_project_memory_documents"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_project_memory_documents_project"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "persisted_tool_result_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_persisted_tool_results"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "interpretation_settings_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_interpretation_settings_documents"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_effective_interpretation_settings"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_interpretation_settings_documents_scope"
    ));
    assert!(object_exists(&conn, "table", "message_tool_references"));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_message_content_blocks"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_message_tool_references_native"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_message_blocks_session_kind"
    ));
    assert!(object_exists(&conn, "table", "canonical_messages"));
    for index in [
        "idx_fact_records_semantic_revision",
        "idx_usage_v2_response_session",
        "idx_usage_v2_response_actor",
        "idx_usage_v2_response_source_generation",
        "idx_runtime_actor_runs_v2_session",
        "idx_runtime_actor_runs_v2_source_generation",
        "idx_runtime_actor_affiliations_v2_actor",
        "idx_runtime_actor_affiliations_v2_target",
        "idx_runtime_actor_affiliations_v2_source_generation",
        "idx_canonical_sessions_source_generation",
        "idx_canonical_messages_source_generation",
        "idx_canonical_runs_source_generation",
        "idx_usage_v2_response_source_generation",
        "idx_run_evidence_compact",
        "idx_run_evidence_decisive",
        "idx_run_evidence_activity_time",
        "idx_run_evidence_source_generation",
        "idx_delegation_assertions_source_generation",
        "idx_delegation_spawn_assertions_source_generation",
    ] {
        assert!(object_exists(&conn, "index", index), "missing {index}");
    }
    for (table, column) in [
        ("source_streams", "raw_retention"),
        ("fact_records", "payload_codec"),
        ("fact_records", "semantic_source_record_id"),
        ("fact_records", "semantic_fact_id"),
        ("fact_records", "semantic_fact_revision_id"),
        ("canonical_messages", "raw_json_codec"),
        ("run_evidence", "evidence_count"),
        ("run_evidence", "last_activity_at"),
    ] {
        let present: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                [column],
                |row| row.get(0),
            )
            .expect("inspect compact payload schema");
        assert_eq!(present, 1, "missing {table}.{column}");
    }
    assert!(!object_exists(
        &conn,
        "index",
        "idx_fact_records_entity_kind"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_message_search_fts"
    ));
    assert!(object_exists(
        &conn,
        "view",
        "canonical_searchable_messages"
    ));
    assert!(object_exists(&conn, "table", "canonical_runs"));
    assert!(object_exists(&conn, "table", "run_evidence"));
    assert!(object_exists(&conn, "table", "observed_run_states"));
    assert!(object_exists(&conn, "table", "presence_assertions"));
    assert!(object_exists(&conn, "table", "canonical_presences"));
    assert!(object_exists(&conn, "table", "delegation_assertions"));
    assert!(object_exists(&conn, "table", "canonical_delegations"));
    assert!(object_exists(
        &conn,
        "table",
        "delegation_metadata_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_delegation_metadata"
    ));
    assert!(object_exists(&conn, "table", "delegation_spawn_assertions"));
    assert!(object_exists(&conn, "table", "canonical_delegation_spawns"));
    assert!(object_exists(&conn, "table", "team_snapshot_assertions"));
    assert!(object_exists(&conn, "table", "team_member_assertions"));
    assert!(object_exists(&conn, "table", "canonical_teams"));
    assert!(object_exists(&conn, "table", "canonical_team_members"));
    assert!(object_exists(
        &conn,
        "table",
        "team_inbox_snapshot_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "team_inbox_message_assertions"
    ));
    assert!(object_exists(&conn, "table", "canonical_team_inboxes"));
    assert!(object_exists(
        &conn,
        "table",
        "canonical_team_inbox_messages"
    ));
    assert!(object_exists(&conn, "table", "task_snapshot_assertions"));
    assert!(object_exists(&conn, "table", "task_item_assertions"));
    assert!(object_exists(&conn, "table", "canonical_task_collections"));
    assert!(object_exists(&conn, "table", "canonical_tasks"));
    assert!(object_exists(&conn, "table", "plan_assertions"));
    assert!(object_exists(&conn, "table", "canonical_plans"));
    assert!(object_exists(
        &conn,
        "table",
        "artifact_snapshot_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "artifact_metadata_assertions"
    ));
    assert!(object_exists(&conn, "table", "artifact_content_assertions"));
    assert!(object_exists(&conn, "table", "canonical_artifacts"));
    assert!(object_exists(
        &conn,
        "table",
        "workflow_snapshot_assertions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "workflow_member_event_assertions"
    ));
    assert!(object_exists(&conn, "table", "canonical_workflows"));
    assert!(object_exists(&conn, "table", "canonical_workflow_members"));
    assert!(object_exists(
        &conn,
        "table",
        "usage_v2_response_contributions"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "usage_v2_qualification_specs"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "usage_v2_qualification_specs"
    ));
    assert!(object_exists(
        &conn,
        "table",
        "usage_v2_response_contributions"
    ));
    assert!(object_exists(&conn, "table", "runtime_actor_runs_v2"));
    assert!(object_exists(
        &conn,
        "table",
        "runtime_actor_affiliations_v2"
    ));
    assert!(object_exists(&conn, "table", "search_fts")); // FTS5 virtual table
    assert!(object_exists(&conn, "index", "idx_messages_session"));
    assert!(object_exists(&conn, "index", "idx_change_log_topic_cursor"));
    assert!(object_exists(
        &conn,
        "index",
        "idx_ingest_commits_retention"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_presence_assertions_source"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_presences_session"
    ));
    assert!(object_exists(&conn, "index", "idx_canonical_runs_commit"));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_messages_run_activity"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_presences_commit"
    ));
    assert!(object_exists(&conn, "index", "idx_canonical_teams_native"));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_team_inboxes_recipient"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_tasks_collection"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_workflow_member_event_assertions_workflow"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_workflow_members_workflow"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_workflow_members_workflow_order"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_workflows_session_activity"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_canonical_delegations_session_activity"
    ));
    assert!(object_exists(
        &conn,
        "index",
        "idx_session_index_entry_assertions_session"
    ));
    assert!(object_exists(&conn, "trigger", "messages_ai"));
    assert!(object_exists(&conn, "trigger", "messages_ad"));
    assert!(object_exists(&conn, "trigger", "messages_au"));
    assert!(object_exists(
        &conn,
        "trigger",
        "canonical_messages_search_ai"
    ));
    assert!(object_exists(
        &conn,
        "trigger",
        "canonical_messages_search_ad"
    ));
    assert!(object_exists(
        &conn,
        "trigger",
        "canonical_messages_search_au"
    ));
    assert!(object_exists(
        &conn,
        "trigger",
        "session_summary_messages_ai"
    ));
    for index in [
        "catalog_projects_source",
        "catalog_sessions_project",
        "catalog_sessions_source",
        "catalog_sessions_activity",
    ] {
        assert!(object_exists(&conn, "index", index), "missing {index}");
    }

    let raw_index_columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('timeline_messages') WHERE name = 'raw_index'",
            [],
            |row| row.get(0),
        )
        .expect("inspect timeline schema");
    assert_eq!(raw_index_columns, 1);
}

#[test]
fn initialize_schema_is_idempotent() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    initialize_schema(&conn).expect("first init");

    // Insert a row we expect to survive the second init, since the
    // version already matches and no wipe should occur.
    conn.execute(
        "INSERT INTO projects (slug, original_path, sessions_index, updated_at) \
         VALUES ('canary', '/tmp/canary', '[]', 123)",
        [],
    )
    .expect("insert canary");

    initialize_schema(&conn).expect("second init");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE slug = 'canary'",
            [],
            |row| row.get(0),
        )
        .expect("count canary");
    assert_eq!(count, 1, "second initialize_schema should not wipe data");

    let version = current_schema_version(&conn).expect("read version");
    assert_eq!(version, Some(SCHEMA_VERSION));
}

#[test]
fn generation_cleanup_queries_use_source_generation_indexes() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    initialize_schema(&conn).expect("initialize schema");

    for (table, index) in [
        ("fact_records", "idx_fact_records_object_generation"),
        (
            "canonical_sessions",
            "idx_canonical_sessions_source_generation",
        ),
        (
            "canonical_messages",
            "idx_canonical_messages_source_generation",
        ),
        ("canonical_runs", "idx_canonical_runs_source_generation"),
        (
            "usage_v2_response_contributions",
            "idx_usage_v2_response_source_generation",
        ),
        ("run_evidence", "idx_run_evidence_source_generation"),
        (
            "delegation_assertions",
            "idx_delegation_assertions_source_generation",
        ),
        (
            "delegation_spawn_assertions",
            "idx_delegation_spawn_assertions_source_generation",
        ),
    ] {
        let sql = format!(
            "EXPLAIN QUERY PLAN SELECT 1 FROM {table} WHERE source_object_id = ?1 AND source_generation <> ?2"
        );
        let mut statement = conn.prepare(&sql).expect("prepare query plan");
        let details = statement
            .query_map([1_i64, 1_i64], |row| row.get::<_, String>(3))
            .expect("read query plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect query plan")
            .join("\n");
        assert!(
            details.contains(index),
            "{table} cleanup did not use {index}: {details}"
        );
    }
}

#[test]
fn run_state_reducers_use_aligned_ordering_indexes() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    initialize_schema(&conn).expect("initialize schema");
    assert!(!object_exists(&conn, "index", "idx_run_evidence_run_order"));

    // Same-version databases from before the aligned reducer indexes may
    // still carry superseded write-only B-trees. Reattaching the owner
    // must remove them as part of idempotent schema repair.
    conn.execute_batch(
        r#"
        CREATE INDEX idx_run_evidence_run_order
          ON run_evidence(run_key, source_generation, cursor_end);
        CREATE INDEX idx_canonical_messages_session_order
          ON canonical_messages(session_key, source_generation, cursor_start);
        CREATE INDEX idx_canonical_message_blocks_run
          ON canonical_message_content_blocks(run_key, message_key, block_ordinal);
        "#,
    )
    .expect("install superseded indexes");
    initialize_schema(&conn).expect("refresh same-version schema");
    assert!(!object_exists(&conn, "index", "idx_run_evidence_run_order"));
    assert!(!object_exists(
        &conn,
        "index",
        "idx_canonical_messages_session_order"
    ));
    assert!(!object_exists(
        &conn,
        "index",
        "idx_canonical_message_blocks_run"
    ));

    for (sql, expected_index) in [
        (
            r#"
            EXPLAIN QUERY PLAN
            SELECT fact_id, evidence_kind, source_time
            FROM run_evidence
            WHERE run_key = ?1
            ORDER BY
              CASE evidence_kind
                WHEN 'terminal_succeeded' THEN 60
                WHEN 'terminal_failed' THEN 60
                WHEN 'terminal_cancelled' THEN 60
                WHEN 'input_requested' THEN 50
                WHEN 'waiting_observed' THEN 45
                WHEN 'run_started' THEN 40
                WHEN 'activity_observed' THEN 35
                WHEN 'run_declared' THEN 20
                ELSE 0
              END DESC,
              CASE evidence_strength
                WHEN 'native_explicit' THEN 40
                WHEN 'native_activity' THEN 30
                WHEN 'presence' THEN 20
                WHEN 'layout' THEN 10
                ELSE 0
              END DESC,
              source_generation DESC, cursor_end DESC,
              last_commit_seq DESC, fact_id DESC
            LIMIT 1
            "#,
            "idx_run_evidence_decisive",
        ),
        (
            r#"
            EXPLAIN QUERY PLAN
            SELECT MAX(last_activity_at) FROM run_evidence
            WHERE run_key = ?1
              AND last_activity_at IS NOT NULL
            "#,
            "idx_run_evidence_activity_time",
        ),
    ] {
        let details = conn
            .prepare(sql)
            .expect("prepare run reducer plan")
            .query_map([b"run".as_slice()], |row| row.get::<_, String>(3))
            .expect("read run reducer plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect run reducer plan")
            .join("\n");
        assert!(
            details.contains(expected_index),
            "run reducer did not use {expected_index}: {details}"
        );
        assert!(
            !details.contains("USE TEMP B-TREE"),
            "run reducer spilled its ordering: {details}"
        );
    }
}

#[test]
fn same_version_attach_refreshes_activity_triggers_and_upserts_stay_safe() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    initialize_schema(&conn).expect("first init");
    conn.execute_batch(
        r#"
        DROP TRIGGER token_activity_messages_ai;
        CREATE TRIGGER token_activity_messages_ai AFTER INSERT ON messages BEGIN SELECT 1; END;
        DROP TRIGGER session_summary_messages_ai;
        CREATE TRIGGER session_summary_messages_ai AFTER INSERT ON messages BEGIN SELECT 1; END;
        "#,
    )
    .expect("install stale trigger");

    initialize_schema(&conn).expect("refresh triggers");
    let trigger_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='token_activity_messages_ai'",
            [],
            |row| row.get(0),
        )
        .expect("read trigger body");
    assert!(trigger_sql.contains("DO NOTHING"));
    let summary_trigger_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='session_summary_messages_ai'",
            [],
            |row| row.get(0),
        )
        .expect("read summary trigger body");
    assert!(summary_trigger_sql.contains("session_summary_dirty"));

    conn.execute_batch(
        r#"
        INSERT INTO messages(project_slug, session_id, msg_index, timestamp, data)
        VALUES ('p', 's', 0, '2026-07-19T00:00:00Z', '{}');
        INSERT INTO messages(project_slug, session_id, msg_index, timestamp, data)
        VALUES ('p', 's', 0, '2026-07-19T00:00:00Z', '{}')
        ON CONFLICT(session_id, msg_index) DO UPDATE SET data=excluded.data;
        "#,
    )
    .expect("outer message upsert must not override dirty-marker conflict handling");
}

#[test]
fn v43_cache_rebuilds_for_message_owned_native_activity_evidence() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(SCHEMA_SQL)
        .expect("install schema fixture");
    conn.execute(
        "INSERT INTO schema_meta (key, value) VALUES ('version', ?1)",
        [(SCHEMA_VERSION - 1).to_string()],
    )
    .expect("set v43 version");
    conn.execute(
        "INSERT INTO projects (slug, original_path, sessions_index, updated_at) \
         VALUES ('preserved', '/tmp/preserved', '[]', 456)",
        [],
    )
    .expect("seed existing cache row");

    initialize_schema(&conn).expect("rebuild v42 cache");

    assert_eq!(
        current_schema_version(&conn).expect("read migrated version"),
        Some(SCHEMA_VERSION)
    );
    let preserved: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE slug = 'preserved'",
            [],
            |row| row.get(0),
        )
        .expect("count preserved row");
    assert_eq!(preserved, 0, "v43 cache retained duplicate activity rows");
    assert!(object_exists(&conn, "index", "idx_run_evidence_compact"));
}

#[test]
fn stale_schema_triggers_wipe_and_rebuild() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    initialize_schema(&conn).expect("first init");

    // Insert a dummy row we expect to be wiped.
    conn.execute(
        "INSERT INTO projects (slug, original_path, sessions_index, updated_at) \
         VALUES ('doomed', '/tmp/doomed', '[]', 456)",
        [],
    )
    .expect("insert doomed");

    // Unknown/older transitions remain wipe-on-stale.
    let stale_version = SCHEMA_VERSION - 2;
    conn.execute(
        "INSERT INTO schema_meta (key, value) VALUES ('version', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [stale_version.to_string()],
    )
    .expect("set stale version");

    // Sanity: version really is stale.
    assert_eq!(
        current_schema_version(&conn).expect("read stale"),
        Some(stale_version)
    );

    initialize_schema(&conn).expect("migrate");

    // Version should now be current.
    assert_eq!(
        current_schema_version(&conn).expect("read after migrate"),
        Some(SCHEMA_VERSION)
    );

    // The doomed row must be gone — wipe-and-rebuild happened.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE slug = 'doomed'",
            [],
            |row| row.get(0),
        )
        .expect("count doomed");
    assert_eq!(count, 0, "stale migration should drop all data");

    // Schema objects should still exist.
    assert!(object_exists(&conn, "table", "messages"));
    assert!(object_exists(&conn, "table", "search_fts"));
    assert!(object_exists(&conn, "trigger", "messages_ai"));
}

/// Bootstrap ingest runs with `foreign_keys = OFF`, so a parent delete —
/// generation replacement on a live corpus, for example — can orphan
/// CASCADE children. Finalization's `foreign_key_check` then fails, and
/// before the poison marker existed every later open re-entered the same
/// finalization and failed identically forever; the only exit was
/// deleting the file by hand.
#[test]
fn failed_bootstrap_validation_poisons_the_marker_and_the_next_open_rebuilds() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("poisoned.db");
    let mut conn = Connection::open(&database).expect("open file database");
    initialize_schema(&conn).expect("first init");
    assert!(begin_query_bootstrap(&mut conn).expect("enter bootstrap"));
    set_bootstrap_ingest_pragmas(&conn).expect("bootstrap pragmas");

    conn.execute_batch(
        "INSERT INTO source_instances (
             source_instance_id, adapter_id, stable_key, display_name,
             adapter_version, adapter_contract_version,
             source_schema_versions_json, capabilities_json,
             discovered_at, last_seen_at
         ) VALUES (1, 'fixture', x'01', 'fixture', '1', 1, '[]', '[]', 1, 1);
         INSERT INTO fact_dependency_reads (
             fact_id, source_instance_id, root_name, object_key, revision
         ) VALUES (x'99', 1, 'root', x'01', x'01');",
    )
    .expect("insert orphaned CASCADE child while foreign keys are off");

    let error = finalize_query_bootstrap(&mut conn).expect_err("validation must fail");
    assert!(
        matches!(error, SchemaError::BootstrapValidation(_)),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        query_bootstrap_state(&conn).expect("read state").as_deref(),
        Some(BOOTSTRAP_STATE_VALIDATION_FAILED),
        "a failed validation must poison the durable marker"
    );

    // The next open wipes and rebuilds instead of failing forever.
    initialize_schema(&conn).expect("reopen rebuilds");
    assert_eq!(
        query_bootstrap_state(&conn).expect("read state after rebuild"),
        None
    );
    assert_eq!(
        current_schema_version(&conn).expect("version after rebuild"),
        Some(SCHEMA_VERSION)
    );
    let orphans: i64 = conn
        .query_row("SELECT COUNT(*) FROM fact_dependency_reads", [], |row| {
            row.get(0)
        })
        .expect("count orphans");
    assert_eq!(orphans, 0, "the poisoned data is gone");
    assert!(finalize_query_bootstrap(&mut conn)
        .expect("no bootstrap is active on the rebuilt database")
        .is_none());
}

#[test]
fn stale_schema_rebuild_reclaims_dead_pages_before_cold_ingest() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("stale.db");
    let conn = Connection::open(&database).expect("open file database");
    initialize_schema(&conn).expect("first init");
    let schema_floor: i64 = conn
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .expect("read empty schema page count");
    conn.execute(
        "INSERT INTO messages (project_slug, session_id, msg_index, data) VALUES ('p', 's', 0, ?1)",
        [vec![b'x'; 4 * 1024 * 1024]],
    )
    .expect("inflate stale cache");
    let before: i64 = conn
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .expect("read inflated page count");
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'version'",
        [SCHEMA_VERSION.saturating_sub(2).to_string()],
    )
    .expect("mark stale");

    initialize_schema(&conn).expect("rebuild and compact");

    let after: i64 = conn
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .expect("read compact page count");
    let freelist_after: i64 = conn
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .expect("read compact freelist count");
    assert!(
        before >= schema_floor + 1_000,
        "fixture did not allocate enough pages: floor {schema_floor}, inflated {before}"
    );
    assert!(
        after <= schema_floor + 8,
        "rebuild did not return near its schema floor: {schema_floor} -> {before} -> {after}"
    );
    assert_eq!(freelist_after, 0, "VACUUM left dead pages on the freelist");
}

#[test]
fn stale_schema_drops_rfc011_foreign_key_graph_in_dependency_order() {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enable foreign keys");
    initialize_schema(&conn).expect("first init");
    conn.execute_batch(
        r#"
        INSERT INTO source_instances (
          source_instance_id, adapter_id, stable_key, display_name,
          adapter_version, adapter_contract_version,
          source_schema_versions_json, capabilities_json,
          discovered_at, last_seen_at
        ) VALUES (1, 'fixture', X'01', 'Fixture', '1.0.0', 1, '[]', '[]', 1, 1);
        INSERT INTO source_streams (
          source_stream_id, source_instance_id, stream_key, driver_kind,
          decoder_key, stream_state
        ) VALUES (1, 1, 'history', 'append_file', 'fixture', 'available');
        INSERT INTO source_objects (
          source_object_id, source_stream_id, object_key, generation,
          committed_cursor, decoder_contract_version, state
        ) VALUES (1, 1, X'02', 1, X'03', 1, 'active');
        INSERT INTO ingest_commits (
          commit_seq, source_instance_id, reason, started_at, committed_at
        ) VALUES (1, 1, 'fixture', 1, 2);
        INSERT INTO change_log (
          commit_seq, ordinal, topic, schema_version, entity_key,
          operation, payload
        ) VALUES (1, 0, 'history.session.changed', 1, X'04', 'upsert', X'05');
        INSERT INTO source_record_errors (
          source_object_id, generation, cursor_start, cursor_end,
          payload_hash, media_type, error_class, error_message,
          adapter_version, contract_version, first_commit_seq
        ) VALUES (
          1, 1, X'01', X'02', X'03', 'application/json', 'fixture',
          'fixture', '1.0.0', 1, 1
        );
        "#,
    )
    .expect("seed RFC 011 graph");
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'version'",
        [SCHEMA_VERSION.saturating_sub(2).to_string()],
    )
    .expect("mark schema stale");

    initialize_schema(&conn).expect("wipe and rebuild RFC 011 graph");

    for table in [
        "source_instances",
        "source_streams",
        "source_objects",
        "ingest_commits",
        "change_log",
        "source_record_errors",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count rebuilt table");
        assert_eq!(count, 0, "{table} should be rebuilt empty");
    }
}

#[test]
fn set_pragmas_enables_wal_on_file_db() {
    // `PRAGMA journal_mode = WAL` is persisted as `memory` on in-memory
    // connections; use a tempfile-backed DB so WAL is actually applied.
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("pragma-test.sqlite");
    let conn = Connection::open(&db_path).expect("open file db");

    set_pragmas(&conn).expect("set pragmas");

    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal_mode");
    assert_eq!(mode.to_lowercase(), "wal");

    // synchronous = NORMAL (1)
    let sync: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .expect("read synchronous");
    assert_eq!(sync, 1);

    // foreign_keys = ON (1)
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .expect("read foreign_keys");
    assert_eq!(fk, 1);

    let cache_size: i64 = conn
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .expect("read cache_size");
    assert_eq!(cache_size, -WRITER_CACHE_KIB);
    let mmap_size: i64 = conn
        .query_row("PRAGMA mmap_size", [], |row| row.get(0))
        .expect("read mmap_size");
    assert_eq!(mmap_size, SQLITE_MMAP_BYTES);
    let checkpoint_pages: i64 = conn
        .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
        .expect("read wal_autocheckpoint");
    assert_eq!(checkpoint_pages, WAL_AUTOCHECKPOINT_PAGES);
}
