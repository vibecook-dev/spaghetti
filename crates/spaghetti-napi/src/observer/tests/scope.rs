//! What the observer follows, and what it refuses to follow.

use std::time::Duration;

use super::support::{
    assistant_record, collect_until, drain_bootstrap, subagent_record, user_record, SessionFixture,
    SESSION,
};
use crate::observer::{ObserverEvent, ObserverHandle};

fn coverage_paths(events: &[ObserverEvent]) -> Vec<String> {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            ObserverEvent::BootstrapComplete(barrier) | ObserverEvent::ResyncComplete(barrier) => {
                Some(
                    barrier
                        .coverage
                        .iter()
                        .map(|entry| entry.object_path.clone())
                        .collect(),
                )
            }
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn a_subagent_transcript_created_after_bootstrap_joins_the_same_scope() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &[user_record("u-1")]);
    let observer = fixture.open();
    let bootstrap = drain_bootstrap(&observer);
    assert!(
        !coverage_paths(&bootstrap)
            .iter()
            .any(|path| path.contains("subagents")),
        "no child existed at bootstrap"
    );

    fixture.append(
        &fixture.subagent("child-1"),
        &[subagent_record("child-1", "c-1")],
    );

    let after = collect_until(&observer, Duration::from_secs(10), |events| {
        events.iter().any(|event| match event {
            ObserverEvent::ActorRun(event) => event.source.object_path.contains("subagents"),
            _ => false,
        })
    });
    assert!(
        after.iter().any(|event| matches!(
            event,
            ObserverEvent::ActorRun(inner) if inner.source.object_path.contains("subagents")
        )),
        "a child transcript that appeared after bootstrap was never followed"
    );
    observer.close();
}

#[test]
fn every_followed_object_names_the_relation_that_admitted_it() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &[user_record("u-1")]);
    fixture.append(
        &fixture.subagent("child-1"),
        &[subagent_record("child-1", "c-1")],
    );

    let observer = fixture.open();
    let bootstrap = drain_bootstrap(&observer);
    let barrier = bootstrap
        .iter()
        .find_map(|event| match event {
            ObserverEvent::BootstrapComplete(barrier) => Some(barrier),
            _ => None,
        })
        .expect("bootstrap barrier");
    assert!(
        barrier
            .coverage
            .iter()
            .all(|entry| !entry.relation_id.is_empty()),
        "an object was opened without a declared relation"
    );
    assert!(
        barrier
            .coverage
            .iter()
            .any(|entry| entry.relation_id == "root-transcript"),
        "the root relation is missing from coverage"
    );
    observer.close();
}

#[test]
fn a_transcript_outside_the_projects_root_is_refused_before_any_watch() {
    let fixture = SessionFixture::new();
    let mut request = fixture.request();
    request.transcript_path = fixture
        .root
        .path()
        .join("elsewhere")
        .join(format!("{SESSION}.jsonl"))
        .to_string_lossy()
        .into_owned();
    let Err(error) = ObserverHandle::open(&request) else {
        panic!("a locator outside the projects root must be refused");
    };
    assert!(
        error.to_string().contains("projects"),
        "unexpected refusal: {error}"
    );
}

#[test]
fn a_declared_session_id_that_disagrees_with_the_locator_fails_attachment() {
    let fixture = SessionFixture::new();
    let mut request = fixture.request();
    request.native_session_id = Some("11111111-2222-3333-4444-555555555555".to_string());
    let Err(error) = ObserverHandle::open(&request) else {
        panic!("a declared session id that disagrees with the locator must be fatal");
    };
    assert!(
        error.to_string().contains("does not match"),
        "unexpected refusal: {error}"
    );
}

#[test]
fn attaching_before_the_root_exists_bootstraps_empty_and_then_follows_it() {
    let fixture = SessionFixture::empty();
    let observer = fixture.open();
    let bootstrap = drain_bootstrap(&observer);
    let barrier = bootstrap
        .iter()
        .find_map(|event| match event {
            ObserverEvent::BootstrapComplete(barrier) => Some(barrier),
            _ => None,
        })
        .expect("an absent root still completes bootstrap");
    assert!(
        !barrier.root_present,
        "the root transcript does not exist yet"
    );
    assert!(
        bootstrap.iter().all(|event| event.is_control()),
        "an empty bootstrap delivered semantic events"
    );

    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 4)],
    );
    let live = collect_until(&observer, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_)))
    });
    assert!(
        live.iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_))),
        "later creation of the root was never observed"
    );
    observer.close();
}

#[test]
fn a_sidecar_joins_the_scope_only_when_evidence_names_it() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );

    let todo = r#"[{"content":"fixture","status":"pending","activeForm":"fixture"}]"#.to_string();
    // The root actor's own sidecar: the transcript decode emits scope-join
    // evidence naming it, so bootstrap reaches it in the same pass set.
    fixture.append_once(&fixture.todo_sidecar(SESSION), &[todo.clone()]);
    // A sidecar for an actor nothing in this session mentions. Reaching it
    // would mean enumerating a global root, which RFC 012D §5 forbids.
    fixture.append_once(&fixture.todo_sidecar("unnamed-actor"), &[todo]);

    let observer = fixture.open();
    let bootstrap = drain_bootstrap(&observer);
    let paths = coverage_paths(&bootstrap);

    assert!(
        paths.contains(&format!("todos/{SESSION}-agent-{SESSION}.json")),
        "the sidecar named by scope-join evidence was not followed: {paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.contains("unnamed-actor")),
        "an unrelated sidecar was opened: {paths:?}"
    );
    observer.close();
}

#[test]
fn one_unreadable_child_does_not_stop_its_siblings() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );
    fixture.append(
        &fixture.subagent("healthy"),
        &[subagent_record("healthy", "c-1")],
    );
    // A directory where a transcript should be: the driver cannot open it.
    std::fs::create_dir_all(fixture.subagent("broken")).expect("broken child");

    let observer = fixture.open();
    let events = collect_until(&observer, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::BootstrapComplete(_)))
            && events.iter().any(|event| match event {
                ObserverEvent::ActorRun(event) => event.source.object_path.contains("healthy"),
                _ => false,
            })
    });

    assert!(
        events.iter().any(|event| match event {
            ObserverEvent::ActorRun(event) => event.source.object_path.contains("healthy"),
            _ => false,
        }),
        "the healthy sibling was never read"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::BootstrapComplete(_))),
        "one unreadable object must not prevent bootstrap from completing"
    );
    observer.close();
}
