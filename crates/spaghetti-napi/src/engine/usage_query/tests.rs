//! Behavioral tests for the response-level usage query.
//!
//! Every case writes real Claude transcript JSONL, reconciles it through the
//! real adapter and engine, and reads the result back through the real query
//! against real SQLite. Nothing is seeded straight into the usage tables.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::adapter::AdapterRegistry;
use crate::claude::ClaudeCodeAdapter;
use crate::engine::{
    EngineOptions, HistoryProjectPageRequest, ReconcileRequest, SpaghettiEngineCore,
};

const SESSION: &str = "11111111-2222-4333-8444-555555555555";

struct Corpus {
    _root: TempDir,
    _database: TempDir,
    engine: Arc<SpaghettiEngineCore>,
    project_id: String,
}

impl Corpus {
    /// Reconcile one transcript through the real Claude adapter.
    fn ingest(lines: &[String]) -> Self {
        let root = TempDir::new().unwrap();
        let project = root.path().join("projects/-tmp-usage");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join(format!("{SESSION}.jsonl")),
            lines.join("\n") + "\n",
        )
        .unwrap();
        let agent_root = root.path().to_path_buf();
        Self::open(root, agent_root)
    }

    fn open(root: TempDir, agent_root: PathBuf) -> Self {
        let database = TempDir::new().unwrap();
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: database.path().join("usage.sqlite"),
                query_workers: Some(1),
                owner_label: Some("usage-query-test".to_string()),
                defer_query_structures: false,
                source_pass_pool: None,
            },
            AdapterRegistry::builder()
                .register(ClaudeCodeAdapter::new())
                .build()
                .unwrap(),
        )
        .unwrap();
        let outcome = engine
            .reconcile_adapter("claude-code", ReconcileRequest::manual(vec![agent_root]))
            .unwrap();
        assert_eq!(
            outcome.records_quarantined, 0,
            "fixture must decode cleanly"
        );
        let projects = engine
            .history_projects(HistoryProjectPageRequest {
                cursor: None,
                limit: 10,
            })
            .unwrap();
        let project_id = projects
            .items
            .first()
            .expect("one project")
            .project_id
            .clone();
        Self {
            _root: root,
            _database: database,
            engine,
            project_id,
        }
    }

    fn usage(&self) -> UsageReport {
        self.engine
            .usage(UsageRequest {
                project_id: self.project_id.clone(),
                session_id: None,
                window: None,
            })
            .unwrap()
    }

    fn usage_window(&self, from: &str, to: &str) -> UsageReport {
        self.engine
            .usage(UsageRequest {
                project_id: self.project_id.clone(),
                session_id: None,
                window: Some(UsageWindow {
                    from: from.to_string(),
                    to: to.to_string(),
                }),
            })
            .unwrap()
    }
}

/// One assistant record with the given usage object verbatim.
fn assistant(message_id: &str, timestamp: &str, usage: &str) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{message_id}-uuid","timestamp":"{timestamp}","sessionId":"{SESSION}","cwd":"/tmp/usage","requestId":"req-{message_id}","message":{{"id":"{message_id}","type":"message","role":"assistant","model":"claude-test","content":[{{"type":"text","text":"ok"}}],"usage":{usage}}}}}"#
    )
}

fn user(timestamp: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"u-{timestamp}","timestamp":"{timestamp}","sessionId":"{SESSION}","cwd":"/tmp/usage","message":{{"role":"user","content":"go"}}}}"#
    )
}

const FULL: &str = r#"{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":5,"cache_read_input_tokens":7}"#;

#[test]
fn evolving_counters_for_one_response_collapse_to_one_contribution() {
    // The same `message.id` reported three times with a growing counter is one
    // response, not three. This is the RFC 012C correction the additive path
    // could not express.
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant(
            "msg-a",
            "2026-04-01T10:00:01.000Z",
            r#"{"input_tokens":10,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}"#,
        ),
        assistant(
            "msg-a",
            "2026-04-01T10:00:02.000Z",
            r#"{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}"#,
        ),
        assistant(
            "msg-a",
            "2026-04-01T10:00:03.000Z",
            r#"{"input_tokens":10,"output_tokens":9,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}"#,
        ),
    ]);

    let report = corpus.usage();
    assert_eq!(report.aggregate.contribution_count, 1);
    assert_eq!(report.aggregate.exact.input_tokens, 10);
    assert_eq!(report.aggregate.exact.output_tokens, 9);
    assert_eq!(report.aggregate.session_count, 1);
}

#[test]
fn a_downward_revision_corrects_the_total_instead_of_being_rejected() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant(
            "msg-a",
            "2026-04-01T10:00:01.000Z",
            r#"{"input_tokens":500,"output_tokens":90,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}"#,
        ),
        assistant(
            "msg-a",
            "2026-04-01T10:00:02.000Z",
            r#"{"input_tokens":120,"output_tokens":30,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}"#,
        ),
    ]);

    let report = corpus.usage();
    assert_eq!(report.aggregate.contribution_count, 1);
    assert_eq!(report.aggregate.exact.input_tokens, 120);
    assert_eq!(report.aggregate.exact.output_tokens, 30);
}

#[test]
fn distinct_responses_each_contribute_once() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant("msg-a", "2026-04-01T10:00:01.000Z", FULL),
        user("2026-04-01T10:00:02.000Z"),
        assistant("msg-b", "2026-04-01T10:00:03.000Z", FULL),
    ]);

    let report = corpus.usage();
    assert_eq!(report.aggregate.contribution_count, 2);
    assert_eq!(report.aggregate.exact.input_tokens, 200);
    assert_eq!(report.aggregate.exact.output_tokens, 40);
    assert_eq!(report.aggregate.exact.cache_creation_tokens, 10);
    assert_eq!(report.aggregate.exact.cache_read_tokens, 14);
    assert_eq!(report.aggregate.exact.component_total_tokens, 264);
    assert_eq!(report.aggregate.quality, "exact");
}

#[test]
fn an_omitted_bucket_stays_unknown_and_is_never_summed_as_zero() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        // No cache buckets at all: their native meaning is not proven here.
        assistant(
            "msg-a",
            "2026-04-01T10:00:01.000Z",
            r#"{"input_tokens":40,"output_tokens":4}"#,
        ),
        assistant("msg-b", "2026-04-01T10:00:02.000Z", FULL),
    ]);

    let report = corpus.usage();
    assert_eq!(report.aggregate.contribution_count, 2);
    // One response is fully exact; the other has known and unknown buckets.
    assert_eq!(report.aggregate.exact_contribution_count, 1);
    assert_eq!(report.aggregate.estimated_contribution_count, 1);
    assert_eq!(report.aggregate.unknown_contribution_count, 0);
    assert_eq!(report.aggregate.quality, "mixed");
    // The unknown cache buckets contribute nothing rather than zero.
    assert_eq!(report.aggregate.combined.cache_creation_tokens, 5);
    assert_eq!(report.aggregate.combined.cache_read_tokens, 7);

    let unknown_cache: Vec<_> = report
        .coverage
        .iter()
        .filter(|entry| entry.value_quality == "unknown" && entry.bucket.starts_with("cache"))
        .collect();
    assert_eq!(unknown_cache.len(), 2);
    for entry in unknown_cache {
        assert_eq!(entry.unknown_reason.as_deref(), Some("missing"));
        assert_eq!(entry.tokens, 0);
        assert_eq!(entry.contribution_count, 1);
        assert_ne!(entry.completeness, "complete");
    }
}

#[test]
fn coverage_names_the_native_field_behind_every_qualified_bucket() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant("msg-a", "2026-04-01T10:00:01.000Z", FULL),
    ]);

    let report = corpus.usage();
    let input = report
        .coverage
        .iter()
        .find(|entry| entry.bucket == "input")
        .expect("input coverage");
    assert_eq!(input.native_field, "message.usage.input_tokens");
    assert_eq!(input.value_quality, "exact");
    assert_eq!(input.authority, "native_response");
    assert_eq!(input.completeness, "complete");
    assert_eq!(input.tokens, 100);
    assert_eq!(input.model.as_deref(), Some("claude-test"));
}

#[test]
fn a_window_splits_days_and_keeps_the_all_time_aggregate() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant("msg-a", "2026-04-01T10:00:01.000Z", FULL),
        user("2026-04-03T10:00:00.000Z"),
        assistant("msg-b", "2026-04-03T10:00:01.000Z", FULL),
    ]);

    let report = corpus.usage_window("2026-04-01", "2026-04-02");
    // The scope aggregate is all-time; only the day series is windowed.
    assert_eq!(report.aggregate.contribution_count, 2);
    let window = report.window.expect("window");
    assert_eq!(window.days.len(), 1);
    assert_eq!(window.days[0].date, "2026-04-01");
    assert_eq!(window.days[0].aggregate.exact.input_tokens, 100);
    assert_eq!(window.days[0].aggregate.contribution_count, 1);
    assert_eq!(window.untimed.aggregate.contribution_count, 0);
}

#[test]
fn a_response_without_a_usable_date_is_reported_untimed_rather_than_dropped() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant("msg-a", "2026-04-01T10:00:01.000Z", FULL),
        // A structurally invalid calendar date cannot own a day.
        assistant("msg-b", "2026-04-31T10:00:01.000Z", FULL),
    ]);

    let report = corpus.usage_window("2026-04-01", "2026-04-30");
    assert_eq!(report.aggregate.contribution_count, 2);
    let window = report.window.expect("window");
    assert_eq!(window.days.len(), 1);
    assert_eq!(window.untimed.aggregate.contribution_count, 1);
    assert_eq!(window.untimed.aggregate.exact.input_tokens, 100);
}

#[test]
fn a_session_outside_the_project_is_rejected_rather_than_answered_empty() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant("msg-a", "2026-04-01T10:00:01.000Z", FULL),
    ]);
    let error = corpus
        .engine
        .usage(UsageRequest {
            project_id: corpus.project_id.clone(),
            session_id: Some("ses_00000000000000000000000000000000".to_string()),
            window: None,
        })
        .unwrap_err();
    assert!(matches!(error, EngineError::InvalidQuery(_)), "{error:?}");
}

#[test]
fn window_bounds_are_calendar_checked_and_bounded() {
    let corpus = Corpus::ingest(&[
        user("2026-04-01T10:00:00.000Z"),
        assistant("msg-a", "2026-04-01T10:00:01.000Z", FULL),
    ]);
    for (from, to) in [
        ("2026-04-31", "2026-05-01"),
        ("2026-05-01", "2026-04-01"),
        ("2020-01-01", "2026-01-01"),
    ] {
        assert!(
            corpus
                .engine
                .usage(UsageRequest {
                    project_id: corpus.project_id.clone(),
                    session_id: None,
                    window: Some(UsageWindow {
                        from: from.to_string(),
                        to: to.to_string(),
                    }),
                })
                .is_err(),
            "{from}..{to} must be rejected"
        );
    }
}

#[test]
fn the_in_repo_claude_fixture_matches_the_independent_oracle() {
    // `scripts/usage_v2_oracle` reduces the same corpus with no knowledge of
    // this schema, adapter, or SQL. Its published totals for `fixtures/small`
    // are the acceptance evidence for the engine's response-level projection.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/small/.claude");
    let root = TempDir::new().unwrap();
    let corpus = Corpus::open(root, fixture);

    let mut input = 0_u64;
    let mut output = 0_u64;
    let mut cache_creation = 0_u64;
    let mut cache_read = 0_u64;
    let mut responses = 0_u64;
    let projects = corpus
        .engine
        .history_projects(HistoryProjectPageRequest {
            cursor: None,
            limit: 200,
        })
        .unwrap();
    for project in &projects.items {
        let report = corpus
            .engine
            .usage(UsageRequest {
                project_id: project.project_id.clone(),
                session_id: None,
                window: None,
            })
            .unwrap();
        input += report.aggregate.combined.input_tokens;
        output += report.aggregate.combined.output_tokens;
        cache_creation += report.aggregate.combined.cache_creation_tokens;
        cache_read += report.aggregate.combined.cache_read_tokens;
        responses += report.aggregate.contribution_count;
    }

    assert_eq!(responses, 119, "oracle finalState.responseCount");
    assert_eq!(input, 252_852, "oracle aggregate.input_tokens.knownValue");
    assert_eq!(output, 57_412, "oracle aggregate.output_tokens.knownValue");
    assert_eq!(
        cache_creation, 11_855,
        "oracle aggregate.cache_creation_input_tokens.knownValue"
    );
    assert_eq!(
        cache_read, 28_700,
        "oracle aggregate.cache_read_input_tokens.knownValue"
    );
}
