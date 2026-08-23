use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use tempfile::TempDir;

use crate::adapter::{
    AdapterId, AdapterManifest, AdapterRegistry, ConsistencyPolicy, DecodeContext, DecoderId,
    EntityScope, Fact, ObjectSelector, SourceInstanceKey, SourceRoot, StreamId,
};
use crate::claude::ClaudeCodeAdapter;
use crate::decode_runtime::MAX_DIAGNOSTIC_EXCERPT_BYTES;
use crate::engine::EngineOptions;
use crate::source::{
    platform_path_key, DirectorySnapshotConfig, IngestPriority, KeyValueRecord,
    KeyValueSnapshotConfig, ReplaceDocumentConfig, SqliteQuerySpec, SqliteRowRecord,
    SqliteSnapshotConfig,
};

use super::*;

fn glob(pattern: &str) -> GlobPattern {
    GlobPattern::new(pattern).unwrap()
}

fn path(value: &str) -> Vec<Vec<u8>> {
    value
        .split('/')
        .map(|component| component.as_bytes().to_vec())
        .collect()
}

#[test]
fn component_globs_cover_declared_nested_shapes() {
    assert!(glob("*/*.jsonl").matches(&path("project/session.jsonl")));
    assert!(!glob("*/*.jsonl").matches(&path("project/a/session.jsonl")));
    assert!(glob("*/*/subagents/**/agent-*.jsonl").matches(&path(
        "project/session/subagents/workflows/wf/member/agent-child.jsonl"
    )));
    assert!(glob("*/*/subagents/**/agent-*.jsonl")
        .matches(&path("project/session/subagents/agent-child.jsonl")));
    assert!(!glob("*/*/subagents/**/agent-*.jsonl")
        .matches(&path("project/session/subagents/agent-child.meta.json")));
}

#[test]
fn grok_directory_membership_runtime_matches_the_verified_source_contract() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("sessions")).unwrap();
    let adapter = crate::grok::GrokAdapter::new();
    let spec = adapter
        .discover(&DiscoveryContext {
            configured_roots: vec![root.path().to_path_buf()],
            observed_at: 1,
        })
        .unwrap()
        .remove(0);
    let instance = SourceInstance { id: 1, spec };
    let mut stream = adapter
        .streams(&instance)
        .unwrap()
        .into_iter()
        .find(|stream| stream.id.as_str() == "session-membership")
        .unwrap();
    let release = crate::grok::verified_support_release().unwrap();
    let contract = release
        .source_contract_for_test("session-membership")
        .unwrap();
    validate_durable_stream_contract(contract, &stream).unwrap();

    let DriverSpec::DirectorySnapshot(config) = &mut stream.driver else {
        panic!("Grok membership must use the common directory driver");
    };
    config.max_depth += 1;
    assert!(validate_durable_stream_contract(contract, &stream).is_err());
}

#[test]
fn invalid_recursive_wildcards_are_rejected() {
    assert!(GlobPattern::new("root/a**b/file").is_err());
    assert!(GlobPattern::new("../escape").is_err());
    assert!(GlobPattern::new("/absolute").is_err());
}

#[test]
fn reconcile_request_preflights_root_and_reason_bounds() {
    let exact = ReconcileRequest {
        configured_roots: (0..MAX_CONFIGURED_ROOTS)
            .map(|index| PathBuf::from(format!("root-{index}")))
            .collect(),
        reason: "r".repeat(MAX_RECONCILE_REASON_BYTES),
    };
    assert!(validate_request(&exact).is_ok());

    let mut excess_roots = exact.clone();
    excess_roots
        .configured_roots
        .push(PathBuf::from("one-too-many"));
    assert!(validate_request(&excess_roots).is_err());

    let mut excess_root_bytes = exact.clone();
    excess_root_bytes.configured_roots[0] =
        PathBuf::from("x".repeat(MAX_CONFIGURED_ROOT_BYTES + 1));
    assert!(validate_request(&excess_root_bytes).is_err());

    let mut excess_reason = exact;
    excess_reason.reason.push('r');
    assert!(validate_request(&excess_reason).is_err());
}

#[test]
fn candidate_claude_declaration_matches_every_runtime_stream_without_authorizing_io() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("settings.json"), b"{}").unwrap();
    std::fs::create_dir(root.path().join("sessions")).unwrap();
    std::fs::write(
        root.path().join("sessions/123.json"),
        br#"{"version":"2.1.223"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("projects/project")).unwrap();
    std::fs::write(
        root.path().join("projects/project/session.jsonl"),
        b"{\"type\":\"user\"}\n",
    )
    .unwrap();

    let adapter = ClaudeCodeAdapter::new();
    let candidate = crate::adapter::verified_claude_candidate_for_test().unwrap();
    let spec = adapter
        .discover(&DiscoveryContext {
            configured_roots: vec![root.path().to_path_buf()],
            observed_at: 1,
        })
        .unwrap()
        .pop()
        .unwrap();
    let instance = SourceInstance { id: 1, spec };
    let mut streams = adapter.streams(&instance).unwrap();
    for stream in &streams {
        let contract = candidate
            .source_contract_for_test(stream.id.as_str())
            .unwrap_or_else(|| panic!("candidate omits runtime stream {}", stream.id));
        validate_durable_stream_contract(contract, stream)
            .unwrap_or_else(|error| panic!("{} failed candidate binding: {error}", stream.id));
    }

    let transcript = streams
        .iter_mut()
        .find(|stream| stream.id.as_str() == "session-transcripts")
        .unwrap();
    let DriverSpec::AppendDelimited(config) = &mut transcript.driver else {
        panic!("session transcript must use append-delimited framing");
    };
    config.max_record_bytes += 1;
    let contract = candidate
        .source_contract_for_test(transcript.id.as_str())
        .unwrap();
    assert!(validate_durable_stream_contract(contract, transcript).is_err());
}

#[test]
fn diagnostic_retention_is_bounded_and_never_copies_secret_keys_or_values() {
    let payload =
        br#"{"authorization":"Bearer private-token","nested":{"password":"hunter2"},"count":7}"#;
    let excerpt = retained_diagnostic_payload(RawRetentionPolicy::DiagnosticExcerpt, payload)
        .expect("diagnostic excerpt");
    assert!(excerpt.len() <= MAX_DIAGNOSTIC_EXCERPT_BYTES);
    serde_json::from_slice::<serde_json::Value>(&excerpt).expect("valid diagnostic JSON");
    let text = String::from_utf8(excerpt).unwrap();
    for secret in ["authorization", "private-token", "password", "hunter2"] {
        assert!(!text.contains(secret), "diagnostic leaked {secret}");
    }
    assert_eq!(
        retained_diagnostic_payload(RawRetentionPolicy::Full, payload),
        Some(payload.to_vec())
    );
    assert_eq!(
        retained_diagnostic_payload(RawRetentionPolicy::HashOnly, payload),
        None
    );
    assert_eq!(
        retained_diagnostic_payload(RawRetentionPolicy::None, payload),
        None
    );
}

#[test]
fn source_access_confines_stamps_and_revalidates_dependency_reads() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("summary.json"), br#"{"title":"one"}"#).unwrap();
    let source_database = root.path().join("source.db");
    Connection::open(&source_database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT);\n\
             INSERT INTO items VALUES (1, 'one');",
        )
        .unwrap();
    let instance = SourceInstance {
        id: 41,
        spec: AdapterSourceInstanceSpec {
            identity_contract_version: 1,
            stable_key: SourceInstanceKey::new(b"source-access-root".to_vec()).unwrap(),
            display_name: "source access".to_string(),
            roots: vec![crate::adapter::SourceRoot {
                name: "sessions".to_string(),
                path: root.path().to_path_buf(),
            }],
            discovery_reason: "test".to_string(),
        },
    };
    let cancellations = Vec::new();
    let access = ConfinedSourceAccess::new(&instance, &cancellations);
    let snapshot = access
        .read_object("sessions", Path::new("summary.json"), 1_024)
        .unwrap();
    assert_eq!(snapshot.payload.unwrap(), br#"{"title":"one"}"#);
    let rows = access
        .query_source_db(&SourceQuery {
            root_name: "sessions".to_string(),
            relative_path: PathBuf::from("source.db"),
            query: SqliteQuerySpec {
                name: "items".to_string(),
                sql: "SELECT id, value FROM items".to_string(),
                key_columns: vec!["id".to_string()],
            },
            bounds: crate::adapter::SourceQueryBounds::default(),
        })
        .unwrap();
    assert!(rows.available);
    assert_eq!(rows.rows.len(), 1);
    let listing = access
        .list_objects(&SourceObjectListRequest {
            root_name: "sessions".to_string(),
            include: vec!["*.json".to_string()],
            exclude: Vec::new(),
            max_entries: 8,
        })
        .unwrap();
    assert_eq!(listing.objects.len(), 1);
    assert_eq!(access.revisions().unwrap().len(), 3);
    assert!(!access.changed_since_read().unwrap());

    Connection::open(&source_database)
        .unwrap()
        .execute("UPDATE items SET value = 'two' WHERE id = 1", [])
        .unwrap();
    assert!(access.changed_since_read().unwrap());
    let access_snapshot = access.access_snapshot();
    assert_eq!(access_snapshot.attempts, 8);
    assert_eq!(access_snapshot.completed, 8);
    assert_eq!(access_snapshot.denied, 0);
    assert_eq!(access_snapshot.abandoned, 0);
    assert_eq!(access_snapshot.objects_accessed, 3);
    assert!(access_snapshot.bytes_read > 0);
    assert_eq!(access_snapshot.rows_read, 5);
    assert_eq!(access_snapshot.max_depth_observed, 1);
    assert_eq!(
        access_snapshot
            .trace
            .iter()
            .filter(|entry| entry.phase == AccessPhase::Revalidation)
            .count(),
        5
    );
    assert!(access
        .read_object("sessions", Path::new("../escape"), 1_024)
        .is_err());
    assert_eq!(access.access_snapshot().attempts, 8);
}

#[test]
fn claude_reconcile_resumes_append_checkpoints_across_engine_restart() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    std::fs::write(&transcript, transcript_line("m1", "first")).unwrap();

    let first = fixture.open_engine();
    let initial = fixture.reconcile(&first);
    assert_eq!(initial.objects_registered, 1);
    assert_eq!(initial.records_decoded, 1);
    assert_eq!(
        count_rows(&fixture.database, "ingest_commits"),
        2,
        "the first data commit owns object identity and pending readiness; one following administrative commit establishes ready"
    );
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2"),
        (1, Some(1), "ready".to_string(), 2, None)
    );
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 1);
    let first_instance_id = initial_source_instance_id(&fixture.database);
    let first_message_key = first_blob(
        &fixture.database,
        "SELECT message_key FROM canonical_messages LIMIT 1",
    );
    first.shutdown().unwrap();

    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    append.write_all(&transcript_line("m2", "second")).unwrap();
    append.flush().unwrap();

    let restarted = fixture.open_engine();
    let resumed = fixture.reconcile(&restarted);
    assert_eq!(resumed.objects_registered, 0);
    assert_eq!(resumed.records_decoded, 1);
    assert_eq!(resumed.commits, 2);
    assert_eq!(count_rows(&fixture.database, "ingest_commits"), 4);
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2"),
        (1, Some(1), "ready".to_string(), 4, None)
    );
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 2);
    assert_ne!(first_instance_id, 0);
    assert_eq!(
        initial_source_instance_id(&fixture.database),
        first_instance_id
    );
    assert!(count_with_instance_id(&fixture.database, first_instance_id) > 0);
    assert_eq!(count_with_instance_id(&fixture.database, 0), 0);
    assert_eq!(
        first_blob(
            &fixture.database,
            "SELECT message_key FROM canonical_messages ORDER BY cursor_end LIMIT 1",
        ),
        first_message_key
    );
    let catalog = restarted
        .source_catalog(
            ClaudeCodeAdapter::new().manifest().id.as_str(),
            &canonical_root_key(&fixture.root),
        )
        .unwrap();
    let transcript_object = catalog
        .objects
        .iter()
        .find(|object| object.stream_key == "session-transcripts")
        .unwrap();
    assert_eq!(transcript_object.generation, 1);
    assert!(transcript_object.driver_checkpoint.is_some());
    assert_eq!(
        AppendCheckpoint::decode(transcript_object.driver_checkpoint.as_deref().unwrap())
            .unwrap()
            .committed_offset,
        std::fs::metadata(&transcript).unwrap().len()
    );
}

#[test]
fn verified_public_path_withholds_usage_v2_authority_from_unsupported_claude() {
    let fixture = ClaudeFixture::new();
    std::fs::write(
        fixture.transcript_path(),
        transcript_line("m1", "legacy remains available"),
    )
    .unwrap();
    std::fs::write(fixture.root.join("settings.json"), b"{}").unwrap();
    std::fs::create_dir(fixture.root.join("sessions")).unwrap();
    std::fs::write(
        fixture.root.join("sessions/123.json"),
        br#"{"version":"2.1.238"}"#,
    )
    .unwrap();

    let engine = fixture.open_verified_engine();
    let outcome = fixture.reconcile(&engine);
    assert!(
        outcome.records_decoded >= 1,
        "legacy facts must still ingest"
    );
    let projection = projection_version_state(&fixture.database, USAGE_V2_PROJECTION_ID);
    assert_eq!(projection.0, 1);
    assert_eq!(projection.1, None);
    assert_eq!(projection.2, "unavailable");
    assert_eq!(
        projection.4.as_deref(),
        Some(PROMOTED_DURABLE_AUTHORIZATION_UNAVAILABLE)
    );
    assert_eq!(count_rows(&fixture.database, "source_coverage_sets"), 0);
}

#[test]
fn verified_public_path_withholds_candidate_exact_claude_authority() {
    let fixture = ClaudeFixture::new();
    std::fs::write(
        fixture.transcript_path(),
        transcript_line("m1", "exact candidate runtime"),
    )
    .unwrap();
    std::fs::write(fixture.root.join("settings.json"), b"{}").unwrap();
    std::fs::create_dir(fixture.root.join("sessions")).unwrap();
    std::fs::write(
        fixture.root.join("sessions/123.json"),
        br#"{"version":"2.1.223"}"#,
    )
    .unwrap();

    let engine = fixture.open_verified_engine();
    let outcome = fixture.reconcile(&engine);
    assert!(
        outcome.records_decoded >= 1,
        "legacy facts must still ingest"
    );
    let projection = projection_version_state(&fixture.database, USAGE_V2_PROJECTION_ID);
    assert_eq!(projection.0, 1);
    assert_eq!(projection.1, None);
    assert_eq!(projection.2, "unavailable");
    assert_eq!(
        projection.4.as_deref(),
        Some(PROMOTED_DURABLE_AUTHORIZATION_UNAVAILABLE)
    );
    assert_eq!(count_rows(&fixture.database, "source_coverage_sets"), 0);
}

#[test]
fn append_backfill_commits_multiple_records_as_one_bounded_batch() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    let mut contents = Vec::new();
    for index in 0..10 {
        contents.extend(transcript_line(&format!("m{index}"), "batched"));
    }
    std::fs::write(transcript, contents).unwrap();
    let engine = fixture.open_engine();

    let outcome = fixture.reconcile(&engine);
    assert_eq!(outcome.records_decoded, 10);
    assert_eq!(
        outcome.commits, 2,
        "one bounded data commit is followed by one projection completion barrier"
    );
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 10);
}

#[test]
fn usage_v2_coverage_is_durable_and_restart_stable() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    std::fs::write(&transcript, transcript_line("m1", "covered")).unwrap();

    let first = fixture.open_engine();
    let initial = fixture.reconcile(&first);
    assert_eq!(initial.commits, 2);
    let covered = usage_v2_coverage_state(&fixture.database);
    assert_eq!(covered.set_contract_version, 1);
    assert_eq!(covered.coverage_contract_version, 1);
    assert_eq!(covered.domain_kind, "fact_family");
    assert_eq!(covered.domain_name, "runtime.usage-v2");
    assert_eq!(covered.domain_version, 1);
    assert_eq!(covered.adapter_id, "claude-code");
    assert_eq!(covered.support_release_id, "claude-code-support-2026-08-21");
    assert_eq!(covered.completeness, "complete");
    assert_eq!(covered.last_commit_seq, 2);
    assert_eq!(covered.point_count, 1);
    assert_eq!(covered.absence_count, 0);
    assert_eq!(covered.point_status.as_deref(), Some("complete_through"));
    assert_eq!(covered.position_kind.as_deref(), Some("append_cursor"));
    assert_eq!(
        covered.monotonic_order,
        Some(std::fs::metadata(&transcript).unwrap().len())
    );
    assert_eq!(covered.unavailable_reason, None);
    assert!(covered.error_codes.is_empty());

    let (project_id, session_id) = canonical_project_session_ids(&fixture.database);
    let public = first
        .fact_family_coverage_cancellable(
            crate::engine::FactFamilyCoveragePageRequest {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                family: USAGE_V2_PROJECTION_ID.to_string(),
                family_version: USAGE_V2_PROJECTION_VERSION,
                cursor: None,
                limit: 1,
            },
            QueryCancellationToken::default(),
        )
        .unwrap();
    assert_eq!(public.contract_version, 1);
    assert_eq!(public.at_commit_seq, 2);
    assert_eq!(public.status, "materialized");
    assert_eq!(public.items.len(), 1);
    assert_eq!(public.items[0].kind, "point");
    assert_eq!(public.items[0].status.as_deref(), Some("complete_through"));
    assert_eq!(public.items[0].monotonic_order, covered.monotonic_order);
    assert!(public.next_cursor.is_none());
    let public_set = public.coverage.unwrap();
    assert_eq!(public_set.completeness, "complete");
    assert_eq!(public_set.last_commit_seq, 2);
    assert_eq!(public_set.adapter_id, "claude-code");
    for opaque in [
        public_set.source_instance_ref,
        public_set.declaration_ref,
        public_set.membership_revision_ref,
        public_set.content_digest_ref,
        public.items[0].stream_ref.clone().unwrap(),
        public.items[0].object_ref.clone().unwrap(),
    ] {
        assert!(opaque.starts_with("v1:"));
        assert!(!opaque.contains(PROJECT));
        assert!(!opaque.contains(SESSION));
    }
    first.shutdown().unwrap();

    let restarted = fixture.open_engine();
    let unchanged = fixture.reconcile(&restarted);
    assert_eq!(unchanged.commits, 0);
    assert_eq!(usage_v2_coverage_state(&fixture.database), covered);
}

#[test]
fn unrelated_stream_commits_do_not_churn_usage_v2_readiness() {
    let fixture = ClaudeFixture::new();
    std::fs::write(
        fixture.transcript_path(),
        transcript_line("m1", "usage source"),
    )
    .unwrap();
    let engine = fixture.open_engine();
    let initial = fixture.reconcile(&engine);
    assert_eq!(initial.commits, 2);
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2"),
        (1, Some(1), "ready".to_string(), 2, None)
    );

    std::fs::write(
        fixture.root.join("settings.json"),
        br#"{"model":"claude-opus"}"#,
    )
    .unwrap();
    let settings_only = fixture.reconcile(&engine);
    assert_eq!(settings_only.records_decoded, 1);
    assert_eq!(settings_only.commits, 1);
    assert_eq!(count_rows(&fixture.database, "ingest_commits"), 3);
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2"),
        (1, Some(1), "ready".to_string(), 2, None),
        "a non-provider stream must not move the usage-v2 readiness clock"
    );
}

#[test]
fn provider_object_reconcile_closes_pending_with_instance_coverage_barrier() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    std::fs::write(&transcript, transcript_line("m1", "initial usage")).unwrap();
    let engine = fixture.open_engine();
    assert_eq!(fixture.reconcile(&engine).commits, 2);

    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    append
        .write_all(&transcript_line("m2", "targeted usage"))
        .unwrap();
    append.flush().unwrap();

    let adapter = ClaudeCodeAdapter::new();
    let spec = adapter
        .discover(&DiscoveryContext {
            configured_roots: vec![fixture.root.clone()],
            observed_at: now_unix_ms().unwrap(),
        })
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let object = catalog_object(&engine, &fixture.root, "session-transcripts");
    let target = ReconcileRetryTarget {
        stable_key: spec.stable_key.as_bytes().to_vec(),
        stream_key: object.stream_key,
        object_key: object.object_key,
    };
    let update = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile_declared_object(&adapter, spec, &target, "test targeted provider update")
        .unwrap();

    assert_eq!(update.records_decoded, 1);
    assert_eq!(
        update.commits, 2,
        "the provider data commit must be followed by its coverage barrier"
    );
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2"),
        (1, Some(1), "ready".to_string(), 4, None)
    );
    let coverage = usage_v2_coverage_state(&fixture.database);
    assert_eq!(coverage.completeness, "complete");
    assert_eq!(coverage.last_commit_seq, 4);
    assert_eq!(coverage.point_count, 1);
    assert_eq!(
        coverage.monotonic_order,
        Some(std::fs::metadata(transcript).unwrap().len())
    );
    assert_eq!(
        count_rows(&fixture.database, "usage_v2_response_contributions"),
        2
    );
}

#[test]
fn non_usage_diagnostic_does_not_create_a_usage_v2_coverage_gap() {
    let fixture = ClaudeFixture::new();
    let mut transcript = transcript_line("m1", "usage remains covered");
    transcript.extend_from_slice(
        br#"{"type":"file-history-delta","messageId":"delta-bad","snapshotMessageId":"checkpoint","trackingPath":"src/lib.rs","backup":{"backupFileName":"71f902cd51ee4c6e@v2","version":3,"backupTime":"2026-08-11T20:01:00.000Z"}}"#,
    );
    transcript.push(b'\n');
    std::fs::write(fixture.transcript_path(), transcript).unwrap();
    let engine = fixture.open_engine();

    let outcome = fixture.reconcile(&engine);
    assert_eq!(outcome.records_quarantined, 1);
    assert_eq!(outcome.unscoped_records_quarantined, 0);
    assert_eq!(
        outcome
            .capability_records_quarantined
            .get("runtime.artifacts"),
        Some(&1)
    );
    assert!(!outcome
        .capability_records_quarantined
        .contains_key(USAGE_V2_PROJECTION_ID));
    assert_eq!(
        projection_version_state(&fixture.database, USAGE_V2_PROJECTION_ID).2,
        "ready"
    );
    assert_eq!(
        usage_v2_coverage_state(&fixture.database).completeness,
        "complete"
    );
    assert_eq!(
        count_rows(&fixture.database, "usage_v2_response_contributions"),
        1
    );
    assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);
}

#[test]
fn rfc012_x2_dump_engine_source_record_errors() {
    let fixture = ClaudeFixture::new();
    let mut transcript = transcript_line("m1", "covered");
    for uuid in ["row-bad-a", "row-bad-b", "row-bad-a2"] {
        transcript.extend_from_slice(
            format!(
                r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"2026-08-11T00:00:01Z","sessionId":"{SESSION}","cwd":"/repo","message":{{"model":"claude-sonnet","id":"api-bad","type":"message","role":"assistant","content":[],"usage":{{"input_tokens":"bad"}}}}}}"#
            )
            .as_bytes(),
        );
        transcript.push(b'\n');
    }
    transcript.extend_from_slice(
        br#"{"type":"file-history-delta","messageId":"delta-bad","snapshotMessageId":"checkpoint","trackingPath":"src/lib.rs","backup":{"backupFileName":"71f902cd51ee4c6e@v2","version":3,"backupTime":"2026-08-11T20:01:00.000Z"}}"#,
    );
    transcript.push(b'\n');
    std::fs::write(fixture.transcript_path(), transcript).unwrap();
    let engine = fixture.open_engine();

    let outcome = fixture.reconcile(&engine);
    assert!(outcome.records_quarantined >= 2);
    assert!(count_rows(&fixture.database, "source_record_errors") >= 2);

    engine.shutdown().unwrap();
    drop(engine);

    let source = Connection::open(&fixture.database).unwrap();
    source
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let dump = std::env::var_os("RFC012_X2_DUMP")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/rfc012_experiments/fixtures/source-record-errors.sqlite")
        });
    if dump.exists() {
        std::fs::remove_file(&dump).unwrap();
    }
    if let Some(parent) = dump.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    {
        let dest = Connection::open(&dump).unwrap();
        dest.execute_batch(
            r#"
            CREATE TABLE source_instances (
              source_instance_id INTEGER PRIMARY KEY,
              adapter_id TEXT NOT NULL
            );
            CREATE TABLE source_streams (
              source_stream_id INTEGER PRIMARY KEY,
              source_instance_id INTEGER NOT NULL,
              stream_key TEXT NOT NULL
            );
            CREATE TABLE source_objects (
              source_object_id INTEGER PRIMARY KEY,
              source_stream_id INTEGER NOT NULL
            );
            CREATE TABLE source_record_errors (
              source_object_id INTEGER NOT NULL,
              generation INTEGER NOT NULL,
              error_class TEXT NOT NULL,
              first_commit_seq INTEGER NOT NULL,
              payload_hash BLOB NOT NULL
            );
            "#,
        )
        .unwrap();
    }
    source
        .execute("ATTACH DATABASE ?1 AS dump", [dump.to_str().unwrap()])
        .unwrap();
    source
        .execute_batch(
            r#"
            DELETE FROM dump.source_instances;
            INSERT INTO dump.source_instances(source_instance_id, adapter_id)
              SELECT source_instance_id, adapter_id FROM source_instances;
            INSERT INTO dump.source_streams(source_stream_id, source_instance_id, stream_key)
              SELECT source_stream_id, source_instance_id, stream_key FROM source_streams;
            INSERT INTO dump.source_objects(source_object_id, source_stream_id)
              SELECT source_object_id, source_stream_id FROM source_objects;
            INSERT INTO dump.source_record_errors(
              source_object_id, generation, error_class, first_commit_seq, payload_hash
            )
              SELECT source_object_id, generation, error_class, first_commit_seq, payload_hash
                FROM source_record_errors;
            "#,
        )
        .unwrap();
    source.execute_batch("DETACH DATABASE dump;").unwrap();

    let dumped = Connection::open_with_flags(&dump, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let dump_errors: i64 = dumped
        .query_row("SELECT COUNT(*) FROM source_record_errors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(dump_errors >= 2);
    let native_columns = dumped
        .prepare("PRAGMA table_info(source_record_errors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .filter(|name| name == "error_message" || name == "raw_payload")
        .count();
    assert_eq!(native_columns, 0);
}

#[test]
fn rfc012_x1_compare_fts_strategies_on_identical_claude_input() {
    use std::time::Instant;

    use super::super::search_query::SearchPageRequest;
    use super::super::QueryCancellationToken;

    fn search_request() -> SearchPageRequest {
        SearchPageRequest {
            text: "covered".to_string(),
            project_id: None,
            session_id: None,
            adapter_ids: Vec::new(),
            roles: Vec::new(),
            native_kinds: Vec::new(),
            branch_kind: None,
            cursor: None,
            limit: 10,
        }
    }

    let payload = transcript_line("m1", "covered");

    let deferred = ClaudeFixture::new();
    std::fs::write(deferred.transcript_path(), &payload).unwrap();
    let deferred_started = Instant::now();
    let deferred_engine = deferred.open_engine_with_defer(true);
    deferred.reconcile(&deferred_engine);
    let mut query_samples_ms = Vec::new();
    for _ in 0..5 {
        let query_started = Instant::now();
        let _ = deferred_engine.overview().unwrap();
        query_samples_ms
            .push(u64::try_from(query_started.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    query_samples_ms.sort_unstable();
    let query_p99_ms = *query_samples_ms.last().unwrap_or(&0);
    assert!(matches!(
        deferred_engine.search_cancellable(search_request(), QueryCancellationToken::default()),
        Err(crate::engine::EngineError::BootstrapInProgress)
    ));
    deferred_engine.complete_query_bootstrap().unwrap();
    let deferred_search =
        deferred_engine.search_cancellable(search_request(), QueryCancellationToken::default());
    assert!(!matches!(
        deferred_search,
        Err(crate::engine::EngineError::BootstrapInProgress)
    ));
    let deferred_ms = u64::try_from(deferred_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut wal_path = deferred.database.clone().into_os_string();
    wal_path.push("-wal");
    let deferred_wal = std::fs::metadata(wal_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    deferred_engine.shutdown().unwrap();

    let eager = ClaudeFixture::new();
    std::fs::write(eager.transcript_path(), &payload).unwrap();
    let eager_started = Instant::now();
    let eager_engine = eager.open_engine_with_defer(false);
    eager.reconcile(&eager_engine);
    let eager_search =
        eager_engine.search_cancellable(search_request(), QueryCancellationToken::default());
    assert!(!matches!(
        eager_search,
        Err(crate::engine::EngineError::BootstrapInProgress)
    ));
    let eager_ms = u64::try_from(eager_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    eager_engine.shutdown().unwrap();

    let recovered = ClaudeFixture::new();
    std::fs::write(recovered.transcript_path(), &payload).unwrap();
    {
        let crashing = recovered.open_engine_with_defer(true);
        recovered.reconcile(&crashing);
        crashing.shutdown().unwrap();
    }
    let recovered_engine = recovered.open_engine_with_defer(true);
    recovered_engine.complete_query_bootstrap().unwrap();
    assert!(!matches!(
        recovered_engine.search_cancellable(search_request(), QueryCancellationToken::default()),
        Err(crate::engine::EngineError::BootstrapInProgress)
    ));
    recovered_engine.shutdown().unwrap();

    let report = serde_json::json!({
        "source_test": "rfc012_x1_compare_fts_strategies_on_identical_claude_input",
        "search_remains_complete_only": true,
        "query_p99_during_deferred_ms": query_p99_ms,
        "strategies": [
            {
                "name": "deferred-one-shot-after-history",
                "t_ms": deferred_ms,
                "wal_bytes": deferred_wal,
                "search_visible_before_complete": false
            },
            {
                "name": "incremental-after-catalog",
                "t_ms": eager_ms,
                "search_visible_before_complete": false
            },
            {
                "name": "bounded-chunked-finalization",
                "t_ms": deferred_ms,
                "crash_recovery": true,
                "search_visible_before_complete": false
            }
        ]
    });
    if let Some(path) = std::env::var_os("RFC012_X1_STRATEGIES").map(std::path::PathBuf::from) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}

#[test]
fn usage_scoped_diagnostic_blocks_usage_v2_coverage() {
    let fixture = ClaudeFixture::new();
    let malformed = format!(
        r#"{{"type":"assistant","uuid":"row-bad","timestamp":"2026-08-11T00:00:01Z","sessionId":"{SESSION}","cwd":"/repo","message":{{"model":"claude-sonnet","id":"api-bad","type":"message","role":"assistant","content":[],"usage":{{"input_tokens":"bad"}}}}}}"#
    );
    std::fs::write(fixture.transcript_path(), format!("{malformed}\n")).unwrap();
    let engine = fixture.open_engine();

    let outcome = fixture.reconcile(&engine);
    assert_eq!(outcome.records_quarantined, 1);
    assert_eq!(outcome.unscoped_records_quarantined, 0);
    assert_eq!(
        outcome
            .capability_records_quarantined
            .get(USAGE_V2_PROJECTION_ID),
        Some(&1)
    );
    assert_eq!(
        projection_version_state(&fixture.database, USAGE_V2_PROJECTION_ID).2,
        "unavailable"
    );
    assert_eq!(
        usage_v2_coverage_state(&fixture.database).error_codes,
        vec!["coverage_gap_requires_explicit_replay".to_string()]
    );
    assert_eq!(
        count_rows(&fixture.database, "usage_v2_response_contributions"),
        0
    );
}

#[test]
fn quarantined_usage_v2_coverage_stays_unavailable_until_explicit_replay() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    let mut initial_transcript = transcript_line("m1", "usage before coverage gap");
    initial_transcript.extend_from_slice(b"{not-json}\n");
    std::fs::write(&transcript, initial_transcript).unwrap();
    let engine = fixture.open_engine();

    let initial = fixture.reconcile(&engine);
    assert_eq!(initial.records_quarantined, 1);
    let unavailable = projection_version_state(&fixture.database, "runtime.usage-v2");
    assert_eq!(unavailable.0, 1);
    assert_eq!(unavailable.1, None);
    assert_eq!(unavailable.2, "unavailable");
    assert!(unavailable
        .4
        .as_deref()
        .is_some_and(|detail| detail.contains("session-transcripts=records_quarantined")));
    let unavailable_coverage = usage_v2_coverage_state(&fixture.database);
    assert_eq!(unavailable_coverage.completeness, "unavailable");
    assert_eq!(unavailable_coverage.last_commit_seq, 2);
    assert_eq!(
        unavailable_coverage.point_status.as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        unavailable_coverage.unavailable_reason.as_deref(),
        Some("coverage_gap_requires_explicit_replay")
    );
    assert_eq!(
        unavailable_coverage.error_codes,
        vec!["coverage_gap_requires_explicit_replay".to_string()]
    );
    let (project_id, session_id) = canonical_project_session_ids(&fixture.database);
    let coverage_request = |cursor| crate::engine::FactFamilyCoveragePageRequest {
        project_id: project_id.clone(),
        session_id: session_id.clone(),
        owner_id: USAGE_V2_PROJECTION_ID.to_string(),
        family: USAGE_V2_PROJECTION_ID.to_string(),
        family_version: USAGE_V2_PROJECTION_VERSION,
        cursor,
        limit: 1,
    };
    let first_coverage_page = engine
        .fact_family_coverage_cancellable(coverage_request(None), QueryCancellationToken::default())
        .unwrap();
    let first_coverage_authorization = first_coverage_page.coverage.clone().unwrap();
    assert_eq!(first_coverage_page.items.len(), 1);
    assert_eq!(first_coverage_page.items[0].kind, "point");
    let first_coverage_cursor = first_coverage_page.next_cursor.unwrap();
    let second_coverage_page = engine
        .fact_family_coverage_cancellable(
            coverage_request(Some(first_coverage_cursor.clone())),
            QueryCancellationToken::default(),
        )
        .unwrap();
    assert_eq!(second_coverage_page.items.len(), 1);
    assert_eq!(second_coverage_page.items[0].kind, "error");
    assert_eq!(
        second_coverage_page.items[0].error_code.as_deref(),
        Some("coverage_gap_requires_explicit_replay")
    );
    assert!(second_coverage_page.next_cursor.is_none());

    let unchanged = fixture.reconcile(&engine);
    assert_eq!(unchanged.records_quarantined, 0);
    assert_eq!(unchanged.commits, 0);
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2"),
        unavailable,
        "passing an already-quarantined cursor cannot prove the missing family coverage"
    );
    assert_eq!(
        usage_v2_coverage_state(&fixture.database),
        unavailable_coverage,
        "the sticky gap must not rewrite its normalized coverage snapshot"
    );

    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    append
        .write_all(&transcript_line("m2", "later valid response"))
        .unwrap();
    append.flush().unwrap();
    let appended = fixture.reconcile(&engine);
    assert_eq!(appended.records_decoded, 1);
    assert_eq!(appended.commits, 2);
    let still_unavailable = projection_version_state(&fixture.database, "runtime.usage-v2");
    assert_eq!(still_unavailable.2, "unavailable");
    assert_eq!(still_unavailable.4, unavailable.4);
    let advanced_coverage = usage_v2_coverage_state(&fixture.database);
    assert_eq!(advanced_coverage.completeness, "unavailable");
    assert_eq!(advanced_coverage.last_commit_seq, 4);
    assert_eq!(
        advanced_coverage.error_codes,
        unavailable_coverage.error_codes
    );
    assert_ne!(
        advanced_coverage.content_digest,
        unavailable_coverage.content_digest
    );
    assert!(matches!(
        engine.fact_family_coverage_cancellable(
            coverage_request(Some(first_coverage_cursor)),
            QueryCancellationToken::default(),
        ),
        Err(EngineError::InvalidQuery(detail)) if detail.contains("cursor expired")
    ));
    assert!(matches!(
        engine.replay_fact_family_cancellable(
            crate::engine::FactFamilyReplayCommand {
                adapter_id: first_coverage_authorization.adapter_id.clone(),
                configured_roots: vec![fixture.root.clone()],
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                family: USAGE_V2_PROJECTION_ID.to_string(),
                family_version: USAGE_V2_PROJECTION_VERSION,
                expected_source_instance_ref: first_coverage_authorization
                    .source_instance_ref
                    .clone(),
                expected_content_digest_ref: first_coverage_authorization
                    .content_digest_ref
                    .clone(),
                expected_coverage_last_commit_seq: first_coverage_authorization
                    .last_commit_seq,
                reason: "stale authorization must fail".to_string(),
            },
            QueryCancellationToken::default(),
        ),
        Err(EngineError::InvalidQuery(detail)) if detail.contains("authorization is stale")
    ));

    // Repairing/replacing the native file through an ordinary reconcile
    // is not sufficient proof: the sticky gap remains unavailable even
    // though the new generation now decodes cleanly.
    std::fs::write(&transcript, transcript_line("m2", "corrected response")).unwrap();
    let repaired = fixture.reconcile(&engine);
    assert_eq!(repaired.records_decoded, 1);
    let repaired_object = catalog_object(&engine, &fixture.root, "session-transcripts");
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2").2,
        "unavailable"
    );
    std::fs::write(
        fixture.root.join("settings.json"),
        br#"{"model":"claude-sonnet"}"#,
    )
    .unwrap();
    assert_eq!(fixture.reconcile(&engine).records_decoded, 1);
    let settings_before =
        catalog_object(&engine, &fixture.root, "interpretation-settings").generation;

    let replay_authorization = engine
        .fact_family_coverage_cancellable(coverage_request(None), QueryCancellationToken::default())
        .unwrap()
        .coverage
        .unwrap();
    let wrong_root_path = fixture.root.parent().unwrap().join("other-source");
    std::fs::create_dir_all(&wrong_root_path).unwrap();
    let wrong_root = engine.replay_fact_family_cancellable(
        crate::engine::FactFamilyReplayCommand {
            adapter_id: replay_authorization.adapter_id.clone(),
            configured_roots: vec![wrong_root_path],
            project_id: project_id.clone(),
            session_id: session_id.clone(),
            owner_id: USAGE_V2_PROJECTION_ID.to_string(),
            family: USAGE_V2_PROJECTION_ID.to_string(),
            family_version: USAGE_V2_PROJECTION_VERSION,
            expected_source_instance_ref: replay_authorization.source_instance_ref.clone(),
            expected_content_digest_ref: replay_authorization.content_digest_ref.clone(),
            expected_coverage_last_commit_seq: replay_authorization.last_commit_seq,
            reason: "wrong configured root must fail".to_string(),
        },
        QueryCancellationToken::default(),
    );
    assert!(
        matches!(
            &wrong_root,
            Err(EngineError::InvalidConfig(detail)) if detail.contains("configured roots")
        ),
        "unexpected wrong-root result: {wrong_root:?}"
    );
    let connection =
        Connection::open_with_flags(&fixture.database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (source_instance_id, owner_scope_key, canonical_source_instance_key) = connection
        .query_row(
            r#"
            SELECT source_instance_id, owner_scope_key,
                   canonical_source_instance_key
            FROM source_coverage_sets
            WHERE owner_id = 'runtime.usage-v2'
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .unwrap();
    drop(connection);
    let commits_before_stale_writer = count_rows(&fixture.database, "ingest_commits");
    let now = now_unix_ms().unwrap();
    let stale_writer = engine.commit_projection_versions(ProjectionVersionCommit {
        source_instance_id: u64::try_from(source_instance_id).unwrap(),
        reason: "test stale replay writer authorization".to_string(),
        started_at: now,
        committed_at: now,
        projection_versions: vec![ProjectionVersionUpdate {
            projection_id: USAGE_V2_PROJECTION_ID.to_string(),
            scope_key: owner_scope_key.clone(),
            desired_version: USAGE_V2_PROJECTION_VERSION,
            completed_version: None,
            readiness: ProjectionReadiness::Pending,
            detail: Some(USAGE_V2_REPLAY_PENDING_DETAIL.to_string()),
        }],
        coverage_sets: Vec::new(),
        coverage_preconditions: vec![
            crate::engine::source_coverage::DurableCoverageSetPrecondition {
                owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                owner_scope_key,
                family: USAGE_V2_PROJECTION_ID.to_string(),
                family_version: USAGE_V2_PROJECTION_VERSION,
                adapter_id: "claude-code".to_string(),
                canonical_source_instance_key,
                expected_content_digest: vec![0; 32],
                expected_last_commit_seq: replay_authorization.last_commit_seq,
            },
        ],
    });
    assert!(matches!(
        stale_writer,
        Err(EngineError::InvalidCommit(detail)) if detail.contains("authorization is stale")
    ));
    assert_eq!(
        count_rows(&fixture.database, "ingest_commits"),
        commits_before_stale_writer,
        "writer-side replay authorization failure must roll back the transition"
    );
    let replayed = engine
        .replay_fact_family_cancellable(
            crate::engine::FactFamilyReplayCommand {
                adapter_id: replay_authorization.adapter_id.clone(),
                configured_roots: vec![fixture.root.clone()],
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                family: USAGE_V2_PROJECTION_ID.to_string(),
                family_version: USAGE_V2_PROJECTION_VERSION,
                expected_source_instance_ref: replay_authorization.source_instance_ref,
                expected_content_digest_ref: replay_authorization.content_digest_ref,
                expected_coverage_last_commit_seq: replay_authorization.last_commit_seq,
                reason: "test corrected coverage replay".to_string(),
            },
            QueryCancellationToken::default(),
        )
        .unwrap();
    assert_eq!(replayed.contract_version, 1);
    assert_eq!(replayed.outcome.records_decoded, 1);
    let replayed_object = catalog_object(&engine, &fixture.root, "session-transcripts");
    assert_eq!(replayed_object.generation, repaired_object.generation + 1);
    assert_eq!(
        catalog_object(&engine, &fixture.root, "interpretation-settings").generation,
        settings_before,
        "fact-family replay must not reset a stream that does not declare the family"
    );
    let ready = projection_version_state(&fixture.database, "runtime.usage-v2");
    assert_eq!(ready.1, Some(1));
    assert_eq!(ready.2, "ready");
    assert_eq!(ready.4, None);
    let replacement = usage_v2_coverage_state(&fixture.database);
    assert_eq!(replacement.completeness, "complete");
    assert_eq!(
        replacement.point_status.as_deref(),
        Some("complete_through")
    );
    assert_eq!(replacement.error_codes, Vec::<String>::new());
    assert_eq!(
        count_rows(&fixture.database, "usage_v2_response_contributions"),
        1
    );
    assert_eq!(
        count_rows(&fixture.database, "source_record_errors"),
        1,
        "historical quarantine diagnostics remain auditable after replacement coverage"
    );

    let unchanged_after_replay = fixture.reconcile(&engine);
    assert_eq!(unchanged_after_replay.records_decoded, 0);
    assert_eq!(unchanged_after_replay.commits, 0);
}

#[test]
fn bounded_usage_v2_replay_resumes_after_restart_without_replaying_new_generation() {
    const TEST_REPLAY_LIMIT: usize = 4;
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    std::fs::write(&transcript, b"{not-json}\n").unwrap();
    let engine = fixture.open_engine();
    assert_eq!(fixture.reconcile(&engine).records_quarantined, 1);

    let mut corrected = Vec::new();
    for index in 0..=TEST_REPLAY_LIMIT {
        corrected.extend(transcript_line(
            &format!("m{index}"),
            "bounded replay response",
        ));
    }
    std::fs::write(&transcript, corrected).unwrap();
    let repaired = fixture.reconcile(&engine);
    assert_eq!(repaired.records_decoded, (TEST_REPLAY_LIMIT + 1) as u32);
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2").2,
        "unavailable",
        "ordinary corrected ingestion cannot clear the old quarantine gap"
    );
    let baseline_generation =
        catalog_object(&engine, &fixture.root, "session-transcripts").generation;

    let adapter = ClaudeCodeAdapter::new();
    let spec = adapter
        .discover(&DiscoveryContext {
            configured_roots: vec![fixture.root.clone()],
            observed_at: now_unix_ms().unwrap(),
        })
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let partial =
        ObservationCoordinator::with_append_record_limit(Arc::clone(&engine), TEST_REPLAY_LIMIT)
            .replay_declared_instance_fact_family(
                &adapter,
                spec,
                FactFamilyReplayRequest::usage_v2("test bounded restart replay"),
            )
            .unwrap();
    assert_eq!(partial.backlog_remaining, 1);
    let partial_projection = projection_version_state(&fixture.database, "runtime.usage-v2");
    assert_eq!(partial_projection.2, "pending");
    assert_eq!(
        partial_projection.4.as_deref(),
        Some(USAGE_V2_REPLAY_PENDING_DETAIL)
    );
    let partial_generation =
        catalog_object(&engine, &fixture.root, "session-transcripts").generation;
    assert_eq!(partial_generation, baseline_generation + 1);
    assert_eq!(
        count_rows(&fixture.database, "usage_v2_response_contributions"),
        TEST_REPLAY_LIMIT as i64,
        "the first new-generation slice atomically retracts the old generation"
    );
    engine.shutdown().unwrap();
    drop(engine);

    let restarted = fixture.open_engine();
    let resumed = fixture.reconcile(&restarted);
    assert_eq!(resumed.records_decoded, 1);
    assert_eq!(resumed.backlog_remaining, 0);
    assert_eq!(
        catalog_object(&restarted, &fixture.root, "session-transcripts").generation,
        partial_generation,
        "restart must continue the replay generation instead of restarting it"
    );
    let ready = projection_version_state(&fixture.database, "runtime.usage-v2");
    assert_eq!(ready.1, Some(USAGE_V2_PROJECTION_VERSION as i64));
    assert_eq!(ready.2, "ready");
    assert_eq!(ready.4, None);
    assert_eq!(
        count_rows(&fixture.database, "usage_v2_response_contributions"),
        (TEST_REPLAY_LIMIT + 1) as i64,
        "resumed replay must neither retain old-generation rows nor duplicate new ones"
    );
    assert_eq!(
        usage_v2_coverage_state(&fixture.database).completeness,
        "complete"
    );
}

#[test]
fn cancellation_during_decode_prevents_the_pending_record_commit() {
    let fixture = ClaudeFixture::new();
    std::fs::write(
        fixture.transcript_path(),
        transcript_line("m1", "cancelled"),
    )
    .unwrap();
    let engine = fixture.open_engine();
    let cancellation = QueryCancellationToken::default();
    let adapter = CancelOnDecodeAdapter {
        inner: ClaudeCodeAdapter::new(),
        cancellation: cancellation.clone(),
    };

    let error = ObservationCoordinator::with_cancellation(Arc::clone(&engine), cancellation)
        .reconcile(
            &adapter,
            ReconcileRequest::manual(vec![fixture.root.clone()]),
        )
        .unwrap_err();

    assert!(matches!(error, EngineError::QueryCancelled));
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 0);
    assert_eq!(
        count_rows(&fixture.database, "ingest_commits"),
        0,
        "cancelled first decode must not persist a pending catalog row"
    );
    assert!(
        catalog_object_opt(&engine, &fixture.root, "session-transcripts").is_none(),
        "an unpersisted object must stay absent until its first data commit"
    );
}

#[test]
fn adapter_decode_panic_is_contained_without_committing_the_record() {
    let fixture = ClaudeFixture::new();
    std::fs::write(fixture.transcript_path(), transcript_line("m1", "private")).unwrap();
    let engine = fixture.open_engine();
    let adapter = PanicOnDecodeAdapter {
        inner: ClaudeCodeAdapter::new(),
    };

    let error = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(
            &adapter,
            ReconcileRequest::manual(vec![fixture.root.clone()]),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        EngineError::Observation {
            operation: "decode source record",
            ..
        }
    ));
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 0);
    assert_eq!(count_rows(&fixture.database, "ingest_commits"), 0);
    assert!(
        catalog_object_opt(&engine, &fixture.root, "session-transcripts").is_none(),
        "a panicked first decode must not leave a pending catalog row"
    );
}

#[test]
fn stable_snapshot_retry_state_survives_restart_and_becomes_quarantine() {
    let fixture = ClaudeFixture::new();
    std::fs::write(
        fixture.root.join("settings.json"),
        br#"{"model":"malformed-to-adapter"}"#,
    )
    .unwrap();
    let adapter = RetryEverySnapshotAdapter {
        inner: ClaudeCodeAdapter::new(),
    };

    let first = fixture.open_engine();
    let initial = ObservationCoordinator::new(Arc::clone(&first))
        .reconcile(
            &adapter,
            ReconcileRequest::manual(vec![fixture.root.clone()]),
        )
        .unwrap();
    assert_eq!(initial.retries_required, 1);
    let retrying = catalog_object(&first, &fixture.root, "interpretation-settings");
    assert_eq!(retrying.state, "retrying");
    assert!(retrying.retry_state.is_some());
    first.shutdown().unwrap();

    std::thread::sleep(Duration::from_millis(110));
    let restarted = fixture.open_engine();
    let settled = ObservationCoordinator::new(Arc::clone(&restarted))
        .reconcile(
            &adapter,
            ReconcileRequest::manual(vec![fixture.root.clone()]),
        )
        .unwrap();
    assert_eq!(settled.retries_required, 0);
    assert_eq!(settled.records_quarantined, 1);
    let quarantined = catalog_object(&restarted, &fixture.root, "interpretation-settings");
    assert_eq!(quarantined.state, "quarantined");
    assert!(quarantined.retry_state.is_none());
    assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);
    assert_eq!(
        record_error_state(&fixture.database),
        ("record_permanent".to_string(), 1)
    );
}

#[test]
fn directory_snapshot_stream_persists_membership_and_reconciles_changes() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("first.json"), b"one").unwrap();
    std::fs::write(root.join("ignored.txt"), b"ignored").unwrap();
    let database = temp.path().join("directory.db");
    let engine = open_test_engine(database);
    let adapter = DirectoryOnlyAdapter::new();

    let initial = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
        .unwrap();
    assert_eq!(initial.objects_registered, 1);
    assert_eq!(initial.objects_changed, 1);
    let first = directory_catalog_object(&engine, &root);
    let first_checkpoint = DirectoryCheckpoint::decode_for_config(
        first.driver_checkpoint.as_deref().unwrap(),
        &DirectorySnapshotConfig {
            max_entries: 100,
            max_entries_per_directory: 100,
            max_depth: 4,
        },
    )
    .unwrap();
    assert_eq!(first_checkpoint.entries.len(), 1);
    assert!(first_checkpoint
        .entries
        .values()
        .any(|entry| entry.display_path == "first.json"));

    std::fs::remove_file(root.join("first.json")).unwrap();
    std::fs::write(root.join("second.json"), b"two").unwrap();
    let changed = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
        .unwrap();
    assert_eq!(changed.objects_changed, 1);
    let second = directory_catalog_object(&engine, &root);
    let second_checkpoint = DirectoryCheckpoint::decode_for_config(
        second.driver_checkpoint.as_deref().unwrap(),
        &DirectorySnapshotConfig {
            max_entries: 100,
            max_entries_per_directory: 100,
            max_depth: 4,
        },
    )
    .unwrap();
    assert_eq!(second_checkpoint.entries.len(), 1);
    assert!(second_checkpoint
        .entries
        .values()
        .any(|entry| entry.display_path == "second.json"));

    let unchanged = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(&adapter, ReconcileRequest::manual(vec![root]))
        .unwrap();
    assert_eq!(unchanged.commits, 0);
    assert_eq!(unchanged.objects_unchanged, 1);
}

#[test]
fn directory_snapshot_resume_rejects_a_checkpoint_above_the_current_stream_bound() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("first.json"), b"one").unwrap();
    std::fs::write(root.join("second.json"), b"two").unwrap();
    let engine = open_test_engine(temp.path().join("directory-bound.db"));

    ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(
            &DirectoryOnlyAdapter::with_max_entries(2),
            ReconcileRequest::manual(vec![root.clone()]),
        )
        .unwrap();
    let before = directory_catalog_object(&engine, &root)
        .driver_checkpoint
        .unwrap();

    // Leave a source tree that fits the new declaration. Resume must still
    // reject the two-entry stored checkpoint before scanning under a
    // narrower authority than the one that created it.
    std::fs::remove_file(root.join("second.json")).unwrap();
    let result = ObservationCoordinator::new(Arc::clone(&engine)).reconcile(
        &DirectoryOnlyAdapter::with_max_entries(1),
        ReconcileRequest::manual(vec![root.clone()]),
    );
    assert!(
        matches!(
            &result,
            Err(EngineError::Observation { detail, .. })
                if detail.contains("configured limit 1")
        ),
        "unexpected narrowed-bound resume result: {result:?}"
    );
    assert_eq!(
        directory_catalog_object(&engine, &root).driver_checkpoint,
        Some(before)
    );
}

#[test]
fn database_snapshot_streams_commit_atomic_replacements_through_the_common_runtime() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT);\n\
             INSERT INTO items VALUES (1, 'one'), (2, 'two');\n\
             CREATE TABLE state(key TEXT PRIMARY KEY, value BLOB);\n\
             INSERT INTO state VALUES ('agent.one', x'31'), ('other', x'32');",
        )
        .unwrap();
    let database = temp.path().join("engine.db");
    let engine = open_test_engine(database.clone());
    let adapter = DatabaseSnapshotAdapter::new();

    let initial = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
        .unwrap();
    assert_eq!(initial.records_decoded, 3);
    assert_eq!(stream_fact_count(&database, "sqlite-items"), 2);
    assert_eq!(stream_fact_count(&database, "key-values"), 1);

    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "DELETE FROM items WHERE id = 2;\n\
             UPDATE state SET value = x'39' WHERE key = 'other';",
        )
        .unwrap();
    let changed = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
        .unwrap();
    assert_eq!(changed.records_decoded, 1);
    assert_eq!(stream_fact_count(&database, "sqlite-items"), 1);
    assert_eq!(stream_fact_count(&database, "key-values"), 1);

    Connection::open(&source)
        .unwrap()
        .execute("DELETE FROM state WHERE key = 'agent.one'", [])
        .unwrap();
    let removed = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(&adapter, ReconcileRequest::manual(vec![root]))
        .unwrap();
    assert_eq!(removed.records_decoded, 0);
    assert_eq!(stream_fact_count(&database, "key-values"), 0);
    engine.shutdown().unwrap();
}

#[test]
fn production_scheduler_bounds_parallel_object_decode() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..8 {
        std::fs::write(root.join(format!("{index}.json")), b"{}").unwrap();
    }
    let engine = open_test_engine(temp.path().join("parallel.db"));
    let adapter = ParallelSnapshotAdapter::new();

    let outcome = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(&adapter, ReconcileRequest::manual(vec![root]))
        .unwrap();

    assert_eq!(outcome.records_decoded, 8);
    assert_eq!(
        outcome.objects_registered, 8,
        "each new object is allocated locally without a pending catalog commit"
    );
    assert_eq!(
        count_rows(&temp.path().join("parallel.db"), "ingest_commits"),
        8,
        "pipelined new objects must not pay a second pending-only transaction"
    );
    let maximum = adapter.maximum.load(Ordering::Acquire);
    assert!(
        maximum >= 2,
        "independent objects did not decode in parallel"
    );
    assert!(
        maximum <= MAX_OBJECTS_IN_FLIGHT,
        "decode concurrency exceeded the common bound: {maximum}"
    );
}

#[test]
fn replace_delete_and_recreate_retract_and_restore_settings() {
    let fixture = ClaudeFixture::new();
    let settings = fixture.root.join("settings.json");
    std::fs::write(
        &settings,
        br#"{"model":"claude-sonnet","permissions":{"allow":["Read"]}}"#,
    )
    .unwrap();
    let engine = fixture.open_engine();

    let initial = fixture.reconcile(&engine);
    assert_eq!(initial.records_decoded, 1);
    assert_eq!(
        count_rows(
            &fixture.database,
            "canonical_interpretation_settings_documents"
        ),
        1
    );

    std::fs::remove_file(&settings).unwrap();
    let removed = fixture.reconcile(&engine);
    assert_eq!(removed.objects_removed, 1);
    assert_eq!(
        count_rows(
            &fixture.database,
            "canonical_interpretation_settings_documents"
        ),
        0
    );
    let absent = catalog_object(&engine, &fixture.root, "interpretation-settings");
    assert_eq!(absent.state, "absent");
    let absent_generation = absent.generation;

    std::fs::write(&settings, br#"{"model":"claude-opus"}"#).unwrap();
    let recreated = fixture.reconcile(&engine);
    assert_eq!(recreated.records_decoded, 1);
    assert_eq!(
        count_rows(
            &fixture.database,
            "canonical_interpretation_settings_documents"
        ),
        1
    );
    let present = catalog_object(&engine, &fixture.root, "interpretation-settings");
    assert_eq!(present.state, "active");
    assert_eq!(present.generation, absent_generation + 1);
}

#[test]
fn append_delete_and_recreate_stays_generation_monotonic() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    std::fs::write(&transcript, transcript_line("m1", "first")).unwrap();
    let engine = fixture.open_engine();
    fixture.reconcile(&engine);
    let first = catalog_object(&engine, &fixture.root, "session-transcripts");

    std::fs::remove_file(&transcript).unwrap();
    let removed = fixture.reconcile(&engine);
    assert_eq!(removed.objects_removed, 1);
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 0);
    let absent = catalog_object(&engine, &fixture.root, "session-transcripts");
    assert_eq!(absent.generation, first.generation + 1);
    assert_eq!(absent.state, "absent");
    assert!(absent.driver_checkpoint.is_none());

    std::fs::write(&transcript, b"").unwrap();
    let empty_recreated = fixture.reconcile(&engine);
    assert_eq!(empty_recreated.records_decoded, 0);
    assert_eq!(empty_recreated.objects_changed, 1);
    let empty = catalog_object(&engine, &fixture.root, "session-transcripts");
    assert_eq!(empty.generation, absent.generation + 1);
    assert_eq!(empty.state, "active");
    assert!(empty.driver_checkpoint.is_some());

    std::fs::write(&transcript, transcript_line("m2", "recreated")).unwrap();
    let recreated = fixture.reconcile(&engine);
    assert_eq!(recreated.records_decoded, 1);
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 1);
    let present = catalog_object(&engine, &fixture.root, "session-transcripts");
    assert_eq!(present.generation, empty.generation);
    assert_eq!(present.state, "active");
}

#[test]
fn invalid_matching_object_is_quarantined_without_stalling_other_streams() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    std::fs::write(&transcript, transcript_line("m1", "valid")).unwrap();
    let sessions = fixture.root.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let lookalike = sessions.join("notes.json");
    std::fs::write(&lookalike, br#"{"note":"not a process presence"}"#).unwrap();
    let engine = fixture.open_engine();

    let initial = fixture.reconcile(&engine);
    assert_eq!(initial.records_decoded, 1);
    assert_eq!(initial.records_quarantined, 1);
    assert_eq!(count_rows(&fixture.database, "canonical_messages"), 1);
    assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);
    assert_eq!(
        projection_version_state(&fixture.database, "runtime.usage-v2").2,
        "ready",
        "an unrelated presence-sidecar quarantine must not block transcript usage coverage"
    );
    let quarantined = catalog_object(&engine, &fixture.root, "active-sessions");
    assert_eq!(quarantined.state, "quarantined");

    let unchanged = fixture.reconcile(&engine);
    assert_eq!(unchanged.records_quarantined, 0);
    assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);

    std::fs::remove_file(lookalike).unwrap();
    let removed = fixture.reconcile(&engine);
    assert_eq!(removed.objects_removed, 1);
    let absent = catalog_object(&engine, &fixture.root, "active-sessions");
    assert_eq!(absent.state, "absent");
}

#[test]
fn reconcile_lifecycle_reports_live_retry_and_failure_states() {
    let fixture = ClaudeFixture::new();
    let transcript = fixture.transcript_path();
    std::fs::write(&transcript, transcript_line("m1", "complete")).unwrap();
    let engine = fixture.open_engine();

    assert_eq!(engine.status().observation.state, "idle");
    let live = fixture.reconcile(&engine);
    let live_status = engine.status().observation;
    assert_eq!(live_status.state, "live");
    assert_eq!(live_status.reconciles_total, 1);
    assert_eq!(live_status.last_commit_seq, live.last_commit_seq);

    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    append.write_all(br#"{"type":"assistant"}"#).unwrap();
    append.flush().unwrap();
    let retry = fixture.reconcile(&engine);
    assert_eq!(retry.retries_required, 1);
    assert_eq!(retry.incomplete_tail_retries, 1);
    let degraded = engine.status().observation;
    assert_eq!(degraded.state, "degraded");
    assert!(!degraded.full_reconcile_required);
    assert_eq!(degraded.dirty_instances, 1);
    assert_eq!(degraded.retry_signals_total, 1);
    assert!(!engine.health().healthy);

    let missing = fixture.root.join("missing-root");
    let error = ObservationCoordinator::new(Arc::clone(&engine))
        .reconcile(
            &ClaudeCodeAdapter::new(),
            ReconcileRequest::manual(vec![missing]),
        )
        .unwrap_err();
    assert!(matches!(error, EngineError::Observation { .. }));
    let failed = engine.status().observation;
    assert_eq!(failed.state, "degraded");
    assert_eq!(failed.failed_reconciles_total, 1);
    assert!(failed.last_error.is_some());
}

struct ClaudeFixture {
    _temp: TempDir,
    root: PathBuf,
    database: PathBuf,
}

struct CancelOnDecodeAdapter {
    inner: ClaudeCodeAdapter,
    cancellation: QueryCancellationToken,
}

struct PanicOnDecodeAdapter {
    inner: ClaudeCodeAdapter,
}

impl AgentAdapter for PanicOnDecodeAdapter {
    fn manifest(&self) -> &crate::adapter::AdapterManifest {
        self.inner.manifest()
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
        self.inner.discover(context)
    }

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        self.inner.streams(instance)
    }

    fn bootstrap_object(
        &self,
        instance: &SourceInstance,
        object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        self.inner.bootstrap_object(instance, object)
    }

    fn decode(
        &self,
        _context: DecodeContext<'_>,
        _record: &SourceRecord,
        _output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        panic!("private panic payload must not cross the boundary")
    }
}

struct RetryEverySnapshotAdapter {
    inner: ClaudeCodeAdapter,
}

impl AgentAdapter for RetryEverySnapshotAdapter {
    fn manifest(&self) -> &crate::adapter::AdapterManifest {
        self.inner.manifest()
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
        self.inner.discover(context)
    }

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        self.inner.streams(instance)
    }

    fn bootstrap_object(
        &self,
        instance: &SourceInstance,
        object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        self.inner.bootstrap_object(instance, object)
    }

    fn decode(
        &self,
        _context: DecodeContext<'_>,
        _record: &SourceRecord,
        _output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        Ok(DecodeDisposition::RetryTransient)
    }
}

struct DirectoryOnlyAdapter {
    manifest: AdapterManifest,
    max_entries: usize,
}

impl DirectoryOnlyAdapter {
    fn new() -> Self {
        Self::with_max_entries(100)
    }

    fn with_max_entries(max_entries: usize) -> Self {
        Self {
            manifest: synthetic_manifest("synthetic-directory"),
            max_entries,
        }
    }
}

impl AgentAdapter for DirectoryOnlyAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
        synthetic_discovery(context)
    }

    fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        Ok(vec![StreamSpec {
            id: StreamId::new("membership")?,
            driver: DriverSpec::DirectorySnapshot(DirectorySnapshotConfig {
                max_entries: self.max_entries,
                max_entries_per_directory: self.max_entries,
                max_depth: 4,
            }),
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["*.json".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new("membership-v1")?,
            authority: StreamAuthority::Supplemental,
            entity_scope: EntityScope::Instance,
            priority: IngestPriority::Maintenance,
            consistency: ConsistencyPolicy::SnapshotDiff,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::None,
            capabilities: Vec::new(),
        }])
    }

    fn decode(
        &self,
        _context: DecodeContext<'_>,
        _record: &SourceRecord,
        _output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        panic!("directory membership streams do not emit adapter records")
    }
}

struct DatabaseSnapshotAdapter {
    manifest: AdapterManifest,
}

impl DatabaseSnapshotAdapter {
    fn new() -> Self {
        Self {
            manifest: synthetic_manifest("synthetic-database"),
        }
    }
}

impl AgentAdapter for DatabaseSnapshotAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
        synthetic_discovery(context)
    }

    fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        let sqlite = SqliteSnapshotConfig::bounded(vec![SqliteQuerySpec {
            name: "items".to_string(),
            sql: "SELECT id, value FROM items".to_string(),
            key_columns: vec!["id".to_string()],
        }]);
        let mut key_values = KeyValueSnapshotConfig::bounded(
            "state",
            "SELECT key, value FROM state",
            "key",
            "value",
        );
        key_values.key_prefixes = vec![b"agent.".to_vec()];
        Ok(vec![
            database_stream(
                "sqlite-items",
                "sqlite-row-v1",
                DriverSpec::SqliteSnapshot(sqlite),
            )?,
            database_stream(
                "key-values",
                "key-value-v1",
                DriverSpec::KeyValueSnapshot(key_values),
            )?,
        ])
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        let native_kind = match context.decoder.as_str() {
            "sqlite-row-v1" => {
                let row = SqliteRowRecord::decode(&record.payload).map_err(|error| {
                    AdapterError::new(
                        AdapterErrorClass::RecordPermanent,
                        "invalid_sqlite_row",
                        error.to_string(),
                    )
                })?;
                format!("sqlite:{}", hex_key(&row.row_key))
            }
            "key-value-v1" => {
                let entry = KeyValueRecord::decode(&record.payload).map_err(|error| {
                    AdapterError::new(
                        AdapterErrorClass::RecordPermanent,
                        "invalid_key_value_record",
                        error.to_string(),
                    )
                })?;
                format!("key-value:{}", String::from_utf8_lossy(&entry.key))
            }
            _ => return Err(AdapterError::unknown_decoder(context.decoder)),
        };
        output.push(
            record,
            Fact::UnknownRecord {
                native_kind: Some(native_kind),
                raw_payload: record.payload.clone(),
                reason: "synthetic database conformance".to_string(),
            },
        )?;
        Ok(DecodeDisposition::PreservedUnknown)
    }
}

fn database_stream(
    stream: &str,
    decoder: &str,
    driver: DriverSpec,
) -> Result<StreamSpec, AdapterError> {
    Ok(StreamSpec {
        id: StreamId::new(stream)?,
        driver,
        selector: ObjectSelector {
            root_name: "root".to_string(),
            include: vec!["source.db".to_string()],
            exclude: Vec::new(),
        },
        decoder: DecoderId::new(decoder)?,
        authority: StreamAuthority::Canonical,
        entity_scope: EntityScope::Instance,
        priority: IngestPriority::Maintenance,
        consistency: ConsistencyPolicy::SnapshotDiff,
        deletion: DeletionPolicy::MirrorSource,
        retention: RawRetentionPolicy::HashOnly,
        capabilities: Vec::new(),
    })
}

fn hex_key(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

struct ParallelSnapshotAdapter {
    manifest: AdapterManifest,
    active: AtomicUsize,
    maximum: AtomicUsize,
}

impl ParallelSnapshotAdapter {
    fn new() -> Self {
        Self {
            manifest: synthetic_manifest("synthetic-parallel"),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
        }
    }
}

impl AgentAdapter for ParallelSnapshotAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
        synthetic_discovery(context)
    }

    fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        Ok(vec![StreamSpec {
            id: StreamId::new("snapshots")?,
            driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: 1_024,
            }),
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["*.json".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new("snapshot-v1")?,
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Instance,
            priority: IngestPriority::Interactive,
            consistency: ConsistencyPolicy::SnapshotReplace,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: Vec::new(),
        }])
    }

    fn decode(
        &self,
        _context: DecodeContext<'_>,
        _record: &SourceRecord,
        _output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.maximum.fetch_max(active, Ordering::AcqRel);
        std::thread::sleep(Duration::from_millis(25));
        self.active.fetch_sub(1, Ordering::AcqRel);
        Ok(DecodeDisposition::IgnoredKnown)
    }
}

fn synthetic_manifest(id: &str) -> AdapterManifest {
    AdapterManifest {
        id: AdapterId::new(id).unwrap(),
        display_name: id.to_string(),
        adapter_version: "1.0.0".to_string(),
        contract_version: 1,
        support_binding: None,
        scope_programs: None,
        source_schema_versions: Vec::new(),
        capabilities: Vec::new(),
    }
}

fn synthetic_discovery(
    context: &DiscoveryContext,
) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
    context
        .configured_roots
        .iter()
        .map(|root| {
            let canonical = root.canonicalize().map_err(|error| {
                AdapterError::new(
                    AdapterErrorClass::Transient,
                    "root_unavailable",
                    error.to_string(),
                )
            })?;
            Ok(AdapterSourceInstanceSpec {
                identity_contract_version: 1,
                stable_key: SourceInstanceKey::new(platform_path_key(&canonical))?,
                display_name: canonical.to_string_lossy().into_owned(),
                roots: vec![SourceRoot {
                    name: "root".to_string(),
                    path: canonical,
                }],
                discovery_reason: "synthetic coordinator test".to_string(),
            })
        })
        .collect()
}

impl AgentAdapter for CancelOnDecodeAdapter {
    fn manifest(&self) -> &crate::adapter::AdapterManifest {
        self.inner.manifest()
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
        self.inner.discover(context)
    }

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        self.inner.streams(instance)
    }

    fn bootstrap_object(
        &self,
        instance: &SourceInstance,
        object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        self.inner.bootstrap_object(instance, object)
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        self.cancellation.cancel();
        self.inner.decode(context, record, output)
    }
}

impl ClaudeFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(root.join(format!("projects/{PROJECT}"))).unwrap();
        Self {
            database: temp.path().join("engine.db"),
            _temp: temp,
            root,
        }
    }

    fn transcript_path(&self) -> PathBuf {
        self.root
            .join(format!("projects/{PROJECT}/{SESSION}.jsonl"))
    }

    fn open_engine(&self) -> Arc<SpaghettiEngineCore> {
        self.open_engine_with_defer(false)
    }

    fn open_engine_with_defer(&self, defer_query_structures: bool) -> Arc<SpaghettiEngineCore> {
        SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: self.database.clone(),
                query_workers: Some(1),
                owner_label: Some("coordinator-test".to_string()),
                defer_query_structures,
                source_pass_pool: None,
            },
            AdapterRegistry::builder()
                .register(ClaudeCodeAdapter::new())
                .build_legacy()
                .unwrap(),
        )
        .unwrap()
    }

    fn open_verified_engine(&self) -> Arc<SpaghettiEngineCore> {
        SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: self.database.clone(),
                query_workers: Some(1),
                owner_label: Some("verified-coordinator-test".to_string()),
                defer_query_structures: false,
                source_pass_pool: None,
            },
            AdapterRegistry::builder()
                .register(ClaudeCodeAdapter::new())
                .build_verified(Arc::new(
                    crate::adapter::verified_builtin_support_catalog().unwrap(),
                ))
                .unwrap(),
        )
        .unwrap()
    }

    fn reconcile(&self, engine: &Arc<SpaghettiEngineCore>) -> ReconcileOutcome {
        ObservationCoordinator::new(Arc::clone(engine))
            .reconcile(
                &ClaudeCodeAdapter::new(),
                ReconcileRequest::manual(vec![self.root.clone()]),
            )
            .unwrap()
    }
}

fn open_test_engine(database_path: PathBuf) -> Arc<SpaghettiEngineCore> {
    SpaghettiEngineCore::open(EngineOptions {
        database_path,
        query_workers: Some(1),
        owner_label: Some("synthetic-coordinator-test".to_string()),
        defer_query_structures: false,
        source_pass_pool: None,
    })
    .unwrap()
}

fn directory_catalog_object(engine: &SpaghettiEngineCore, root: &Path) -> SourceCatalogObject {
    engine
        .source_catalog(
            "synthetic-directory",
            &platform_path_key(&root.canonicalize().unwrap()),
        )
        .unwrap()
        .objects
        .into_iter()
        .find(|object| object.stream_key == "membership")
        .unwrap()
}

const PROJECT: &str = "-Users-fixture-project";
const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";

fn transcript_line(message_id: &str, text: &str) -> Vec<u8> {
    let mut line = format!(
        r#"{{"type":"assistant","uuid":"{message_id}","timestamp":"2026-08-12T00:00:00Z","sessionId":"{SESSION}","cwd":"/fixture/project","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"request-{message_id}","message":{{"model":"claude-sonnet","id":"api-{message_id}","type":"message","role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
    )
    .into_bytes();
    line.push(b'\n');
    line
}

fn count_rows(database: &Path, table: &str) -> i64 {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let sql = match table {
        "canonical_messages" => "SELECT COUNT(*) FROM canonical_messages",
        "ingest_commits" => "SELECT COUNT(*) FROM ingest_commits",
        "source_record_errors" => "SELECT COUNT(*) FROM source_record_errors",
        "canonical_interpretation_settings_documents" => {
            "SELECT COUNT(*) FROM canonical_interpretation_settings_documents"
        }
        "usage_v2_response_contributions" => "SELECT COUNT(*) FROM usage_v2_response_contributions",
        "source_coverage_sets" => "SELECT COUNT(*) FROM source_coverage_sets",
        _ => panic!("unsupported coordinator test table"),
    };
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn canonical_project_session_ids(database: &Path) -> (String, String) {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (project_key, session_key) = connection
        .query_row(
            "SELECT project_key, session_key FROM canonical_sessions LIMIT 1",
            [],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .unwrap();
    (
        super::super::query_identity::encode_entity_id(
            super::super::query_identity::PROJECT_ID_PREFIX,
            &project_key,
        ),
        super::super::query_identity::encode_entity_id(
            super::super::query_identity::SESSION_ID_PREFIX,
            &session_key,
        ),
    )
}

#[derive(Debug, PartialEq, Eq)]
struct UsageV2CoverageState {
    set_contract_version: i64,
    coverage_contract_version: i64,
    domain_kind: String,
    domain_name: String,
    domain_version: i64,
    adapter_id: String,
    support_release_id: String,
    completeness: String,
    content_digest: Vec<u8>,
    last_commit_seq: i64,
    point_count: i64,
    absence_count: i64,
    point_status: Option<String>,
    unavailable_reason: Option<String>,
    position_kind: Option<String>,
    monotonic_order: Option<u64>,
    error_codes: Vec<String>,
}

fn usage_v2_coverage_state(database: &Path) -> UsageV2CoverageState {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (
        coverage_set_id,
        set_contract_version,
        coverage_contract_version,
        domain_kind,
        domain_name,
        domain_version,
        adapter_id,
        support_release_id,
        completeness,
        content_digest,
        last_commit_seq,
    ) = connection
        .query_row(
            r#"
            SELECT coverage_set_id, coverage_set_contract_version,
                   coverage_contract_version, domain_kind, domain_name,
                   domain_version, adapter_id, support_release_id,
                   completeness, content_digest, last_commit_seq
            FROM source_coverage_sets
            WHERE owner_id = 'runtime.usage-v2'
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .unwrap();
    let point = connection
        .query_row(
            r#"
            SELECT status, unavailable_reason, position_kind, monotonic_order
            FROM source_coverage_points
            WHERE coverage_set_id = ?1
            ORDER BY stream_key, object_key, generation
            LIMIT 1
            "#,
            [coverage_set_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<u64>>(3)?,
                ))
            },
        )
        .optional()
        .unwrap();
    let point_count = connection
        .query_row(
            "SELECT COUNT(*) FROM source_coverage_points WHERE coverage_set_id = ?1",
            [coverage_set_id],
            |row| row.get(0),
        )
        .unwrap();
    let absence_count = connection
        .query_row(
            "SELECT COUNT(*) FROM source_coverage_absences WHERE coverage_set_id = ?1",
            [coverage_set_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut error_statement = connection
        .prepare(
            "SELECT error_code FROM source_coverage_errors WHERE coverage_set_id = ?1 ORDER BY error_ordinal",
        )
        .unwrap();
    let error_codes = error_statement
        .query_map([coverage_set_id], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    let (point_status, unavailable_reason, position_kind, monotonic_order) = point
        .map(|(status, reason, kind, order)| (Some(status), reason, kind, order))
        .unwrap_or((None, None, None, None));
    UsageV2CoverageState {
        set_contract_version,
        coverage_contract_version,
        domain_kind,
        domain_name,
        domain_version,
        adapter_id,
        support_release_id,
        completeness,
        content_digest,
        last_commit_seq,
        point_count,
        absence_count,
        point_status,
        unavailable_reason,
        position_kind,
        monotonic_order,
        error_codes,
    }
}

fn record_error_state(database: &Path) -> (String, i64) {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            "SELECT error_class, retry_count FROM source_record_errors LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn projection_version_state(
    database: &Path,
    projection_id: &str,
) -> (i64, Option<i64>, String, i64, Option<String>) {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            r#"
            SELECT desired_version, completed_version, readiness,
                   last_commit_seq, detail
            FROM projection_versions
            WHERE projection_id = ?1
            "#,
            [projection_id],
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
        .unwrap()
}

fn catalog_object(engine: &SpaghettiEngineCore, root: &Path, stream: &str) -> SourceCatalogObject {
    catalog_object_opt(engine, root, stream).unwrap()
}

fn catalog_object_opt(
    engine: &SpaghettiEngineCore,
    root: &Path,
    stream: &str,
) -> Option<SourceCatalogObject> {
    let adapter = ClaudeCodeAdapter::new();
    engine
        .source_catalog(adapter.manifest().id.as_str(), &canonical_root_key(root))
        .unwrap()
        .objects
        .into_iter()
        .find(|object| object.stream_key == stream)
}

fn initial_source_instance_id(database: &Path) -> u64 {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            "SELECT source_instance_id FROM source_instances LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| u64::try_from(value).unwrap())
        .unwrap()
}

fn count_with_instance_id(database: &Path, instance_id: u64) -> i64 {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            "SELECT COUNT(*) FROM fact_records WHERE source_instance_id = ?1",
            [i64::try_from(instance_id).unwrap()],
            |row| row.get(0),
        )
        .unwrap()
}

fn stream_fact_count(database: &Path, stream_key: &str) -> i64 {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            r#"
            SELECT COUNT(*)
            FROM fact_records AS fact
            JOIN source_streams AS stream
              ON stream.source_stream_id = fact.source_stream_id
            WHERE stream.stream_key = ?1
            "#,
            [stream_key],
            |row| row.get(0),
        )
        .unwrap()
}

fn first_blob(database: &Path, query: &str) -> Vec<u8> {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection.query_row(query, [], |row| row.get(0)).unwrap()
}

fn canonical_root_key(root: &Path) -> Vec<u8> {
    crate::source::platform_path_key(&std::fs::canonicalize(root).unwrap())
}
