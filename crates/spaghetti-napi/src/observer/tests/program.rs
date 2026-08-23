//! The observer follows the adapter's declared scope program, and only that.

use std::collections::BTreeSet;

use super::support::{
    assistant_record, subagent_record, workflow_journal_record, SessionFixture, SESSION,
};
use crate::adapter::{
    AgentAdapter, CanonicalFactId, CanonicalSourceInstanceKey, FactRevisionId, ScopeJoinEvidence,
    ScopeJoinIdentityInput, ScopeJoinParameterSet, ScopeJoinUpdate, SemanticRevisionRef,
};
use crate::claude::ClaudeCodeAdapter;
use crate::observer::scope::{resolve_members, JoinedLocators, ScopeProgram, StreamCatalog};

/// The declared session-rooted program, as the adapter ships it.
fn program() -> (ScopeProgram, StreamCatalog, ClaudeCodeAdapter) {
    let adapter = ClaudeCodeAdapter::new();
    let declared = adapter
        .manifest()
        .scope_programs
        .as_ref()
        .expect("adapter declares scope programs")
        .programs
        .iter()
        .find(|program| program.root_entity_kind == "session")
        .expect("a session-rooted program is declared");
    let program = ScopeProgram::select(declared).expect("program is usable");
    (program, StreamCatalog::new(Vec::new()), adapter)
}

fn catalog_for(adapter: &ClaudeCodeAdapter, fixture: &SessionFixture) -> StreamCatalog {
    let instance = fixture.source_instance(adapter);
    StreamCatalog::new(adapter.streams(&instance).expect("declared streams"))
}

fn evidence() -> ScopeJoinEvidence {
    let instance_key =
        CanonicalSourceInstanceKey::derive(1, b"scope-program-test").expect("instance key");
    let fact_id = CanonicalFactId::native("claude-code", &instance_key, "session", b"fixture")
        .expect("fact id");
    let revision = FactRevisionId::derive(&fact_id, 1, b"fixture-revision").expect("revision");
    ScopeJoinEvidence::new(fact_id, SemanticRevisionRef::new(revision))
}

fn join(relation_id: &str, inputs: &[(&str, &str)]) -> ScopeJoinUpdate {
    let parameters = ScopeJoinParameterSet::new(
        inputs
            .iter()
            .map(|(name, value)| {
                ScopeJoinIdentityInput::from_utf8(*name, value).expect("identity input")
            })
            .collect(),
    )
    .expect("parameter set");
    ScopeJoinUpdate::new(relation_id, vec![evidence()], vec![parameters])
        .expect("scope join update")
}

#[test]
fn every_declared_relation_kind_resolves_to_the_paths_it_declares() {
    let fixture = SessionFixture::new();
    fixture.append_once(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );
    // One object per declared relation kind that the fixture can create:
    // a child directory (subagent transcripts), a sibling (its metadata), a
    // workflow child directory, and a workflow journal.
    fixture.append_once(
        &fixture.subagent("child-1"),
        &[subagent_record("child-1", "c-1")],
    );
    fixture.write_subagent_metadata("child-1");
    fixture.append_once(
        &fixture.workflow_child("wf-1", "child-2"),
        &[subagent_record("child-2", "c-2")],
    );
    fixture.append_once(
        &fixture.workflow_journal("wf-1"),
        &[workflow_journal_record("wf-1")],
    );
    fixture.write_workflow_run("wf-1");
    fixture.write_todo(SESSION);
    fixture.write_team("team-1", "member-1");

    let (program, _, adapter) = program();
    let catalog = catalog_for(&adapter, &fixture);
    let request = fixture.request().resolve("claude-code").expect("request");

    // Evidence naming the relations whose locators are evidence-bound.
    let mut joined = JoinedLocators::default();
    joined.apply(&[
        join(
            "todo-snapshot-from-evidence",
            &[("native-session-id", SESSION), ("native-actor-id", SESSION)],
        ),
        join(
            "team-config-from-evidence",
            &[("observed-team-name", "team-1")],
        ),
        join(
            "team-inbox-from-evidence",
            &[
                ("observed-team-name", "team-1"),
                ("observed-recipient", "member-1"),
            ],
        ),
        join(
            "workflow-journals",
            &[
                ("project-key", fixture.project()),
                ("native-session-id", SESSION),
                ("observed-workflow-id", "wf-1"),
            ],
        ),
        join(
            "workflow-child-transcripts",
            &[
                ("project-key", fixture.project()),
                ("native-session-id", SESSION),
                ("observed-workflow-id", "wf-1"),
            ],
        ),
        join(
            "workflow-records",
            &[
                ("project-key", fixture.project()),
                ("native-session-id", SESSION),
                ("observed-workflow-id", "wf-1"),
            ],
        ),
    ]);

    let members = resolve_members(&request, &program, &catalog, &joined);
    let paths: BTreeSet<String> = members
        .iter()
        .map(|member| {
            format!(
                "{}:{}",
                member.key.root_name,
                member.key.relative_path.to_string_lossy()
            )
        })
        .collect();

    let project = fixture.project();
    for expected in [
        format!("projects:{project}/{SESSION}.jsonl"),
        format!("projects:{project}/{SESSION}/subagents/agent-child-1.jsonl"),
        format!("projects:{project}/{SESSION}/subagents/agent-child-1.jsonl.meta.json"),
        format!("projects:{project}/{SESSION}/subagents/workflows/wf-1/agent-child-2.jsonl"),
        format!("projects:{project}/{SESSION}/subagents/workflows/wf-1/journal.jsonl"),
        format!("projects:{project}/{SESSION}/workflows/wf_wf-1.json"),
        format!("home:todos/{SESSION}-agent-{SESSION}.json"),
        "teams:team-1/config.json".to_string(),
        "teams:team-1/inboxes/member-1.json".to_string(),
    ] {
        assert!(
            paths.contains(&expected),
            "declared relation did not resolve {expected}; resolved {paths:#?}"
        );
    }

    // Every member names the relation that admitted it.
    assert!(
        members.iter().all(|member| !member.relation_id.is_empty()),
        "every member is attributable to a declared relation"
    );
}

#[test]
fn an_identity_value_that_would_escape_the_root_is_refused() {
    let fixture = SessionFixture::new();
    fixture.append_once(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );
    let (program, _, adapter) = program();
    let catalog = catalog_for(&adapter, &fixture);
    let request = fixture.request().resolve("claude-code").expect("request");

    // A decoder that offered a traversal, a separator, or an absolute path as
    // an identity value would otherwise render a locator outside the access
    // root. The confinement law in `source::access` is what refuses them.
    let mut joined = JoinedLocators::default();
    for hostile in ["../../escape", "a/b", "/etc/passwd", "..", "a\\b"] {
        joined.apply(&[join(
            "todo-snapshot-from-evidence",
            &[("native-session-id", SESSION), ("native-actor-id", hostile)],
        )]);
    }

    let members = resolve_members(&request, &program, &catalog, &joined);
    for member in &members {
        let rendered = member.key.relative_path.to_string_lossy();
        assert!(
            !member.key.relative_path.is_absolute(),
            "a locator rendered as an absolute path: {rendered}"
        );
        // A traversal is a whole component, not a substring: a file literally
        // named `x-agent-..json` is confined, and refusing it would be wrong.
        assert!(
            member
                .key
                .relative_path
                .components()
                .all(|component| !matches!(component, std::path::Component::ParentDir)),
            "a locator escaped its access root: {rendered}"
        );
    }
    // Every value carrying a separator is refused outright, so those bindings
    // produce no object at all.
    assert_eq!(
        members
            .iter()
            .filter(|member| member.relation_id == "todo-snapshot-from-evidence")
            .count(),
        1,
        "only the one value that is a legal filename component should resolve"
    );
}

#[test]
fn evidence_for_an_undeclared_relation_is_never_followed() {
    let fixture = SessionFixture::new();
    fixture.append_once(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );
    // Relations the manifest deliberately leaves out: nothing emits evidence
    // for them, and evidence that arrived anyway must not open an object.
    fixture.write_plan("some-plan");

    let (program, _, adapter) = program();
    let catalog = catalog_for(&adapter, &fixture);
    let request = fixture.request().resolve("claude-code").expect("request");

    let mut joined = JoinedLocators::default();
    joined.apply(&[
        join("plan-document-from-evidence", &[("plan-slug", "some-plan")]),
        join(
            "task-item-from-evidence",
            &[("collection", "c"), ("task-id", "t")],
        ),
    ]);

    let members = resolve_members(&request, &program, &catalog, &joined);
    assert!(
        !members
            .iter()
            .any(|member| member.key.relative_path.starts_with("plans")
                || member.key.relative_path.starts_with("tasks")),
        "an undeclared relation was followed: {:#?}",
        members
            .iter()
            .map(|member| member.key.relative_path.clone())
            .collect::<Vec<_>>()
    );
}
