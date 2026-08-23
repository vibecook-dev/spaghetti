//! What the observer follows, and what it refuses to follow.

use std::time::Duration;

use super::support::{
    assistant_record, collect_until, drain_bootstrap, open_observer, subagent_record, user_record,
    SessionFixture, SESSION,
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
    let Err(error) = open_observer(&request) else {
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
    let Err(error) = open_observer(&request) else {
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

#[test]
fn a_sidecar_the_decoder_cannot_map_arrives_as_bounded_evidence() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );
    // The root actor's todo sidecar joins the scope through declared evidence.
    // Its content here is a shape the todo decoder cannot map, which is exactly
    // the case that must surface rather than disappear.
    fixture.append_once(
        &fixture.todo_sidecar(SESSION),
        &[r#"{"unexpected":"shape"}"#.to_string()],
    );

    let observer = fixture.open();
    let events = drain_bootstrap(&observer);

    let unknown = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::UnknownEvidence(event) => Some(event),
            _ => None,
        })
        .expect("an unmappable in-scope document must surface as bounded evidence");
    assert!(
        unknown.observed_bytes > 0,
        "unknown evidence reports how much was not interpreted"
    );
    assert!(
        !unknown.source.record_digest.is_empty(),
        "unknown evidence names the record it stands for"
    );
    assert!(
        unknown.source.object_path.contains("todos"),
        "unknown evidence names the object it came from"
    );
    // No native values travel with it.
    let encoded = serde_json::to_string(unknown).expect("serialize");
    assert!(
        !encoded.contains("unexpected"),
        "bounded evidence must not carry native content"
    );
    observer.close();
}

#[test]
fn a_change_with_no_watcher_notification_is_still_picked_up_by_the_sweep() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );

    // No filesystem watches at all: the only thing that can notice the append
    // below is the bounded reconciliation sweep. RFC 012D §11 makes that the
    // authority precisely because notifications get dropped and coalesced.
    let observer = ObserverHandle::open_unwatched(
        &fixture.request(),
        std::sync::Arc::new(crate::claude::ClaudeCodeAdapter::new()),
    )
    .expect("observer attaches");
    let bootstrap = drain_bootstrap(&observer);
    assert!(
        bootstrap
            .iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_))),
        "bootstrap should read the existing record"
    );

    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-2", "resp-2", 9)],
    );
    let live = collect_until(&observer, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_)))
    });
    assert!(
        live.iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_))),
        "the sweep must pick up an append no watcher reported"
    );
    observer.close();
}

#[test]
fn a_pass_reads_what_changed_rather_than_the_whole_scope() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );
    // Enough children that a full sweep is clearly distinguishable from a
    // targeted pass.
    for index in 0..40 {
        let agent = format!("child-{index}");
        fixture.append(
            &fixture.subagent(&agent),
            &[subagent_record(&agent, &format!("c-{index}"))],
        );
    }

    // A long poll interval makes the sweep cadence long too, so anything read
    // during the window below was read because it was flagged, not swept.
    let mut request = fixture.request();
    request.poll_interval_ms = Some(2_000);
    let observer = open_observer(&request).expect("observer attaches");
    let bootstrap = drain_bootstrap(&observer);
    assert!(
        bootstrap
            .iter()
            .any(|event| matches!(event, ObserverEvent::BootstrapComplete(_))),
        "bootstrap completes"
    );
    let after_bootstrap = observer.status().object_reads;

    // One child changes. A whole-scope pass would read all 41+ objects.
    fixture.append(
        &fixture.subagent("child-7"),
        &[subagent_record("child-7", "c-7-second")],
    );
    collect_until(&observer, Duration::from_secs(10), |events| {
        events.iter().any(|event| match event {
            ObserverEvent::UsageV2(event) => event.source.object_path.contains("child-7"),
            _ => false,
        })
    });

    let reads = observer.status().object_reads - after_bootstrap;
    // The property is that cost tracks what changed, not how big the scope is:
    // 41 members are in scope and one of them moved. A handful of reads is
    // expected — a flagged object is re-read whatever `stat` says, and the
    // filesystem may report the same change more than once — but a
    // whole-scope pass would be an order of magnitude more.
    assert!(
        reads < 15,
        "one changed child in a 41-member scope should not cost a whole-scope \
         pass: {reads} object reads"
    );
    observer.close();
}

#[test]
fn a_burst_across_many_children_coalesces_into_few_passes() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 5)],
    );
    for index in 0..30 {
        let agent = format!("child-{index}");
        fixture.append(
            &fixture.subagent(&agent),
            &[subagent_record(&agent, &format!("c-{index}"))],
        );
    }

    let mut request = fixture.request();
    request.poll_interval_ms = Some(2_000);
    let observer = open_observer(&request).expect("observer attaches");
    drain_bootstrap(&observer);
    let after_bootstrap = observer.status().object_reads;

    // Thirty children change at once. Each is one object that genuinely needs
    // reading; the guarantee is that the burst does not multiply into a pass
    // per notification across the whole scope.
    for index in 0..30 {
        let agent = format!("child-{index}");
        fixture.append(
            &fixture.subagent(&agent),
            &[subagent_record(&agent, &format!("c-{index}-second"))],
        );
    }
    collect_until(&observer, Duration::from_secs(15), |events| {
        events
            .iter()
            .filter(|event| matches!(event, ObserverEvent::UsageV2(_)))
            .count()
            >= 30
    });

    let reads = observer.status().object_reads - after_bootstrap;
    assert!(
        // Measured: exactly 30 — one read per genuinely changed object.
        reads <= 45,
        "a 30-child burst should cost one read per change, not a sweep per \
         notification: {reads} object reads"
    );
    observer.close();
}
