//! Behavioral tests for catalog-first startup.
//!
//! Every test builds a real Claude source tree in a temp directory, opens a
//! real engine on a real SQLite file, and asserts on what a caller can
//! actually observe. Nothing here checks a digest or round-trips a struct.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use crate::adapter::AdapterRegistry;
use crate::claude::ClaudeCodeAdapter;

use super::super::{
    ConfiguredObservationSource, EngineOptions, HistoryProjectPageRequest, ReconcileRequest,
    SpaghettiEngineCore,
};
use super::readiness::ReadinessState;
use super::{
    CatalogPageBounds, CatalogProjectPageRequest, CatalogSessionPageRequest, CatalogState,
};

const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
const SESSION_B: &str = "22222222-2222-4222-8222-222222222222";
const SESSION_INDEX_ONLY: &str = "33333333-3333-4333-8333-333333333333";
const SESSION_DIRECTORY_ONLY: &str = "77777777-7777-4777-8777-777777777777";

/// One assistant turn the Claude decoder accepts, so a transcript that reaches
/// the history path produces a canonical session rather than a decode error.
fn transcript_line(session_id: &str, cwd: &str) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{session_id}","sessionId":"{session_id}","timestamp":"2026-08-01T00:00:00.000Z","cwd":"{cwd}","message":{{"id":"msg_1","role":"assistant","model":"claude-test","content":[{{"type":"text","text":"hello"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
    )
}

/// A Claude source root: two transcript-backed sessions in one project and a
/// third session that exists only in the native index.
fn claude_tree(base: &Path) -> PathBuf {
    let root = base.join("claude");
    let alpha = root.join("projects").join("-Users-dev-alpha");
    let beta = root.join("projects").join("-Users-dev-beta");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();

    std::fs::write(
        alpha.join(format!("{SESSION_A}.jsonl")),
        format!("{}\n", transcript_line(SESSION_A, "/Users/dev/alpha")),
    )
    .unwrap();
    std::fs::write(
        beta.join(format!("{SESSION_B}.jsonl")),
        format!("{}\n", transcript_line(SESSION_B, "/Users/dev/beta")),
    )
    .unwrap();
    // A session that exists only as its side-data directory: no transcript of
    // its own, but unmistakably a session of this project.
    std::fs::create_dir_all(beta.join(SESSION_DIRECTORY_ONLY).join("subagents")).unwrap();
    // A session with both a transcript and a side-data directory.
    std::fs::create_dir_all(alpha.join(SESSION_A).join("tool-results")).unwrap();
    std::fs::write(
        alpha.join("sessions-index.json"),
        session_index(&[
            (SESSION_A, "first prompt for A"),
            (SESSION_INDEX_ONLY, "a session with no transcript"),
        ]),
    )
    .unwrap();
    root
}

fn session_index(entries: &[(&str, &str)]) -> String {
    let entries = entries
        .iter()
        .map(|(id, prompt)| {
            format!(
                r#"{{"sessionId":"{id}","fullPath":"/dev/null","fileMtime":1,"firstPrompt":"{prompt}","messageCount":7,"created":"2026-07-01T00:00:00.000Z","modified":"2026-07-02T00:00:00.000Z","gitBranch":"main","projectPath":"/Users/dev/alpha","isSidechain":false}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"version":1,"entries":[{entries}]}}"#)
}

fn open_engine(database_path: PathBuf) -> std::sync::Arc<SpaghettiEngineCore> {
    SpaghettiEngineCore::open_with_registry(
        EngineOptions {
            database_path,
            query_workers: Some(1),
            owner_label: Some("catalog-test".to_string()),
            defer_query_structures: false,
            source_pass_pool: None,
        },
        AdapterRegistry::builder()
            .register(ClaudeCodeAdapter::new())
            .build()
            .unwrap(),
    )
    .unwrap()
}

fn configured(root: &Path) -> ConfiguredObservationSource {
    ConfiguredObservationSource::new(
        "claude-code",
        vec![root.to_path_buf()],
        "catalog-test".to_string(),
    )
}

fn projects(engine: &SpaghettiEngineCore) -> super::CatalogProjectPage {
    engine
        .catalog_projects(CatalogProjectPageRequest {
            bounds: CatalogPageBounds::default(),
            adapter_ids: Vec::new(),
        })
        .unwrap()
}

fn sessions(engine: &SpaghettiEngineCore) -> super::CatalogSessionPage {
    engine
        .catalog_sessions(CatalogSessionPageRequest {
            bounds: CatalogPageBounds::default(),
            project_id: None,
            adapter_ids: Vec::new(),
        })
        .unwrap()
}

#[test]
fn cold_catalog_is_complete_before_any_history_row_exists() {
    let temp = TempDir::new().unwrap();
    let root = claude_tree(temp.path());
    let engine = open_engine(temp.path().join("cold.db"));

    engine.discover_source_catalog(&configured(&root)).unwrap();

    let projects = projects(&engine);
    assert_eq!(projects.projects.len(), 2, "both project directories");
    assert!(projects.at_commit_seq > 0);

    let sessions = sessions(&engine);
    assert_eq!(
        sessions.sessions.len(),
        4,
        "two transcripts, the index-only session, and the directory-only session"
    );

    // The whole point: this is true with zero decoded history.
    let history = engine
        .history_projects(HistoryProjectPageRequest {
            cursor: None,
            limit: 50,
        })
        .unwrap();
    assert!(
        history.items.is_empty(),
        "catalog must not wait for the history path"
    );

    for session in &sessions.sessions {
        assert_eq!(
            session.catalog_state,
            CatalogState::Discovered,
            "nothing is decoded yet"
        );
    }

    // Discoverable is not the same fact as transcript-backed.
    let index_only = sessions
        .sessions
        .iter()
        .find(|session| session.native_session_id.as_deref() == Some(SESSION_INDEX_ONLY))
        .expect("index-only session is catalog-visible");
    assert!(!index_only.transcript_present);
    assert_eq!(index_only.association_basis, "native_project_index");
    assert_eq!(index_only.native_message_count, Some(7));
    assert!(index_only.title.is_some());

    // A session whose only evidence is its side-data directory is still a
    // catalog member; a third of real projects are discoverable only this way.
    let directory_only = sessions
        .sessions
        .iter()
        .find(|session| session.native_session_id.as_deref() == Some(SESSION_DIRECTORY_ONLY))
        .expect("directory-only session is catalog-visible");
    assert!(!directory_only.transcript_present);
    assert_eq!(directory_only.association_basis, "session_directory");

    let backed = sessions
        .sessions
        .iter()
        .find(|session| session.native_session_id.as_deref() == Some(SESSION_A))
        .expect("transcript-backed session is catalog-visible");
    assert!(
        backed.transcript_present,
        "a session with both a transcript and a side-data directory keeps its transcript"
    );
    assert_eq!(backed.association_basis, "session_directory");
    assert_eq!(backed.association_quality, "exact");

    let readiness = engine.readiness().unwrap();
    assert_eq!(readiness.catalog.state, ReadinessState::Ready);
    assert_eq!(readiness.history.state, ReadinessState::Indexing);
    assert!(readiness
        .history
        .detail
        .as_deref()
        .is_some_and(|detail| detail.starts_with("0 of 2")));

    engine.shutdown().unwrap();
}

#[test]
fn warm_start_serves_the_last_committed_catalog_before_rescanning() {
    let temp = TempDir::new().unwrap();
    let root = claude_tree(temp.path());
    let database = temp.path().join("warm.db");

    let engine = open_engine(database.clone());
    engine.discover_source_catalog(&configured(&root)).unwrap();
    let cold_refs = sessions(&engine)
        .sessions
        .into_iter()
        .map(|session| session.external_ref)
        .collect::<Vec<_>>();
    engine.shutdown().unwrap();

    // Reopen and read *without* rescanning: the rows are already committed.
    let engine = open_engine(database);
    let warm = sessions(&engine);
    assert_eq!(warm.sessions.len(), 4, "warm start serves the last catalog");

    let warm_refs = warm
        .sessions
        .iter()
        .map(|session| session.external_ref.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        warm_refs, cold_refs,
        "ExternalEntityRef is stable across restarts"
    );

    let resolved = engine.resolve_catalog_entity(warm_refs[0].clone()).unwrap();
    assert!(matches!(
        resolved,
        super::CatalogEntityResolution::LiveSession(_)
    ));

    engine.shutdown().unwrap();
}

#[test]
fn a_source_that_cannot_be_read_is_marked_degraded_and_keeps_its_rows() {
    let temp = TempDir::new().unwrap();
    let root = claude_tree(temp.path());
    let engine = open_engine(temp.path().join("degraded.db"));
    engine.discover_source_catalog(&configured(&root)).unwrap();
    assert_eq!(sessions(&engine).sessions.len(), 4);

    // Corrupt the native index. Discovery keeps every row it already proved
    // and reports the source as degraded rather than emptying the library.
    std::fs::write(
        root.join("projects")
            .join("-Users-dev-alpha")
            .join("sessions-index.json"),
        b"{ this is not json",
    )
    .unwrap();
    engine.discover_source_catalog(&configured(&root)).unwrap();

    let readiness = engine.readiness().unwrap();
    assert_eq!(readiness.catalog.state, ReadinessState::Degraded);
    assert!(readiness.catalog.detail.is_some());

    let after = sessions(&engine);
    assert!(
        after.sessions.len() >= 3,
        "a degraded pass retracts nothing it could not disprove"
    );
    assert!(after.sessions.iter().all(|session| session.degraded));

    engine.shutdown().unwrap();
}

#[test]
fn a_rescan_picks_up_a_new_transcript_and_retracts_a_deleted_one() {
    let temp = TempDir::new().unwrap();
    let root = claude_tree(temp.path());
    let engine = open_engine(temp.path().join("rescan.db"));
    engine.discover_source_catalog(&configured(&root)).unwrap();
    assert_eq!(sessions(&engine).sessions.len(), 4);

    let gamma = root.join("projects").join("-Users-dev-gamma");
    std::fs::create_dir_all(&gamma).unwrap();
    let added = "44444444-4444-4444-8444-444444444444";
    std::fs::write(
        gamma.join(format!("{added}.jsonl")),
        format!("{}\n", transcript_line(added, "/Users/dev/gamma")),
    )
    .unwrap();
    std::fs::remove_file(
        root.join("projects")
            .join("-Users-dev-beta")
            .join(format!("{SESSION_B}.jsonl")),
    )
    .unwrap();

    engine.rescan_catalog(Some("claude-code")).unwrap();

    let after = sessions(&engine);
    let ids = after
        .sessions
        .iter()
        .filter_map(|session| session.native_session_id.clone())
        .collect::<Vec<_>>();
    assert!(ids.iter().any(|id| id == added), "new transcript appears");
    assert!(
        !ids.iter().any(|id| id == SESSION_B),
        "a deleted transcript is retracted by a complete pass"
    );

    engine.shutdown().unwrap();
}

#[test]
fn an_unchanged_rescan_costs_no_commit() {
    let temp = TempDir::new().unwrap();
    let root = claude_tree(temp.path());
    let engine = open_engine(temp.path().join("idempotent.db"));
    engine.discover_source_catalog(&configured(&root)).unwrap();

    let after_first = engine.overview().unwrap().commit_seq;
    engine.rescan_catalog(None).unwrap();
    engine.rescan_catalog(None).unwrap();
    assert_eq!(
        engine.overview().unwrap().commit_seq,
        after_first,
        "rescanning an unchanged surface must not advance the watermark that open cursors bind to"
    );
    assert_eq!(sessions(&engine).sessions.len(), 4);

    engine.shutdown().unwrap();
}

#[test]
fn a_session_claimed_by_two_projects_reports_the_conflict() {
    let temp = TempDir::new().unwrap();
    let root = claude_tree(temp.path());
    // The beta project's index claims SESSION_A, which physically lives in
    // alpha. The directory association is exact and wins; the index claim is
    // retained as a conflict rather than merged away.
    std::fs::write(
        root.join("projects")
            .join("-Users-dev-beta")
            .join("sessions-index.json"),
        session_index(&[(SESSION_A, "claimed by beta too")]),
    )
    .unwrap();

    let engine = open_engine(temp.path().join("conflict.db"));
    engine.discover_source_catalog(&configured(&root)).unwrap();

    let sessions = sessions(&engine);
    let contested = sessions
        .sessions
        .iter()
        .find(|session| session.native_session_id.as_deref() == Some(SESSION_A))
        .expect("contested session is listed");
    assert_eq!(
        contested.association_basis, "session_directory",
        "the exact association wins"
    );
    assert_eq!(
        contested.identity_conflicts.len(),
        1,
        "the competing claim stays queryable"
    );
    assert_eq!(
        contested.identity_conflicts[0].competing_native_project_key,
        "-Users-dev-beta"
    );
    assert_eq!(
        contested.identity_conflicts[0].basis,
        "native_project_index"
    );

    engine.shutdown().unwrap();
}

#[test]
fn pagination_is_stable_across_a_commit_between_pages() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("claude");
    let project = root.join("projects").join("-Users-dev-paged");
    std::fs::create_dir_all(&project).unwrap();
    for index in 0..7_u32 {
        let session = format!("5555555{index}-5555-4555-8555-555555555555");
        std::fs::write(
            project.join(format!("{session}.jsonl")),
            format!("{}\n", transcript_line(&session, "/Users/dev/paged")),
        )
        .unwrap();
    }

    let engine = open_engine(temp.path().join("paged.db"));
    engine.discover_source_catalog(&configured(&root)).unwrap();

    let first = engine
        .catalog_sessions(CatalogSessionPageRequest {
            bounds: CatalogPageBounds {
                cursor: None,
                limit: 3,
            },
            project_id: None,
            adapter_ids: Vec::new(),
        })
        .unwrap();
    assert_eq!(first.sessions.len(), 3);
    let cursor = first.cursor.clone().expect("more pages remain");

    // Commit more rows between pages. The cursor is bound to the first page's
    // snapshot, so the continuation must not see them.
    let extra = "66666666-6666-4666-8666-666666666666";
    std::fs::write(
        project.join(format!("{extra}.jsonl")),
        format!("{}\n", transcript_line(extra, "/Users/dev/paged")),
    )
    .unwrap();
    engine.rescan_catalog(None).unwrap();

    let second = engine
        .catalog_sessions(CatalogSessionPageRequest {
            bounds: CatalogPageBounds {
                cursor: Some(cursor),
                limit: 3,
            },
            project_id: None,
            adapter_ids: Vec::new(),
        })
        .unwrap();

    let first_ids = first
        .sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<Vec<_>>();
    for session in &second.sessions {
        assert!(
            !first_ids.contains(&session.session_id),
            "a continuation page never repeats a row"
        );
        assert!(
            session.native_session_id.as_deref() != Some(extra),
            "a row committed after page one cannot enter this listing"
        );
    }

    engine.shutdown().unwrap();
}

#[test]
fn history_convergence_promotes_the_catalog_state_of_a_transcript() {
    let temp = TempDir::new().unwrap();
    let root = claude_tree(temp.path());
    let database = temp.path().join("converge.db");
    let engine = open_engine(database.clone());
    engine.discover_source_catalog(&configured(&root)).unwrap();

    assert!(sessions(&engine)
        .sessions
        .iter()
        .all(|session| session.catalog_state == CatalogState::Discovered));

    // Run the durable history path for the same source. Catalog rows keep
    // their identity and are promoted in place, because both sides derive the
    // session key from the same native material.
    engine
        .reconcile_adapter(
            "claude-code",
            ReconcileRequest {
                configured_roots: vec![root.clone()],
                reason: "catalog-test".to_string(),
            },
        )
        .unwrap();

    let after = sessions(&engine);
    let promoted = after
        .sessions
        .iter()
        .filter(|session| session.catalog_state > CatalogState::Discovered)
        .count();
    assert!(
        promoted >= 2,
        "both transcript-backed sessions converge; got {promoted}"
    );

    let index_only = after
        .sessions
        .iter()
        .find(|session| session.native_session_id.as_deref() == Some(SESSION_INDEX_ONLY))
        .expect("index-only session survives history convergence");
    assert_eq!(
        index_only.catalog_state,
        CatalogState::Discovered,
        "a metadata-only session never gains fabricated history"
    );

    assert_no_legacy_catalog_tables(&database);
    engine.shutdown().unwrap();
}

/// The 012B publication tables must be gone, not merely unused.
fn assert_no_legacy_catalog_tables(database: &Path) {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for table in [
        "catalog_snapshots",
        "catalog_snapshot_entries",
        "catalog_build_state",
        "catalog_coverage_plans",
    ] {
        let present: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(present, 0, "{table} must not exist");
    }
}

/// Measure catalog-first startup against a real corpus.
///
/// Ignored by default: it needs a populated native root, which only exists on
/// a developer machine. Run with
/// `CATALOG_CORPUS_ROOT=~/.claude cargo test -p spaghetti-napi catalog_startup_on_a_real_corpus -- --ignored --nocapture`.
///
/// It prints counts and durations only — never a path, name, id, or prompt.
#[test]
#[ignore = "requires a populated native root"]
fn catalog_startup_on_a_real_corpus() {
    let Ok(root) = std::env::var("CATALOG_CORPUS_ROOT") else {
        panic!("set CATALOG_CORPUS_ROOT to a native agent root");
    };
    let root = PathBuf::from(root);
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("corpus.db");

    let cold_open = std::time::Instant::now();
    let engine = open_engine(database.clone());
    let cold_open_ms = cold_open.elapsed().as_millis();
    let discovery = std::time::Instant::now();
    engine.discover_source_catalog(&configured(&root)).unwrap();
    let cold_discovery_ms = discovery.elapsed().as_millis();

    let page = std::time::Instant::now();
    let first_page = engine
        .catalog_projects(CatalogProjectPageRequest {
            bounds: CatalogPageBounds::default(),
            adapter_ids: Vec::new(),
        })
        .unwrap();
    let first_page_ms = page.elapsed().as_millis();
    let readiness_started = std::time::Instant::now();
    let readiness = engine.readiness().unwrap();
    let readiness_ms = readiness_started.elapsed().as_millis();

    let mut projects = 0_usize;
    let mut cursor = None;
    loop {
        let page = engine
            .catalog_projects(CatalogProjectPageRequest {
                bounds: CatalogPageBounds { cursor, limit: 500 },
                adapter_ids: Vec::new(),
            })
            .unwrap();
        projects += page.projects.len();
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    let mut sessions = 0_usize;
    let mut cursor = None;
    loop {
        let page = engine
            .catalog_sessions(CatalogSessionPageRequest {
                bounds: CatalogPageBounds { cursor, limit: 500 },
                project_id: None,
                adapter_ids: Vec::new(),
            })
            .unwrap();
        sessions += page.sessions.len();
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    engine.shutdown().unwrap();

    // Warm: reopen the same database and serve the last committed catalog
    // with no rescan at all.
    let warm_open = std::time::Instant::now();
    let engine = open_engine(database);
    let warm_page = engine
        .catalog_projects(CatalogProjectPageRequest {
            bounds: CatalogPageBounds::default(),
            adapter_ids: Vec::new(),
        })
        .unwrap();
    let warm_ms = warm_open.elapsed().as_millis();
    let warm_rescan = std::time::Instant::now();
    engine.rescan_catalog(None).unwrap();
    let warm_rescan_ms = warm_rescan.elapsed().as_millis();
    engine.shutdown().unwrap();

    println!("cold_engine_open_ms={cold_open_ms}");
    println!("cold_discovery_ms={cold_discovery_ms}");
    println!("cold_catalog_ready_ms={}", cold_open_ms + cold_discovery_ms);
    println!("cold_first_page_ms={first_page_ms}");
    println!("readiness_ms={readiness_ms}");
    println!("warm_open_to_first_page_ms={warm_ms}");
    println!("warm_unchanged_rescan_ms={warm_rescan_ms}");
    println!("projects={projects}");
    println!("sessions={sessions}");
    println!("first_page_projects={}", first_page.projects.len());
    println!("warm_first_page_projects={}", warm_page.projects.len());
    println!("catalog_state={}", readiness.catalog.state.as_str());
    println!("history_state={}", readiness.history.state.as_str());
}
