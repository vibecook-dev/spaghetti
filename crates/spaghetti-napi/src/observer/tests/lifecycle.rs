//! Bootstrap, append, and the source transitions an append tail must survive.

use std::time::Duration;

use super::support::{
    assistant_record, collect_until, compact_assistant_record, drain_bootstrap,
    drain_bootstrap_within, semantic_ids, user_record, SessionFixture,
};
use crate::observer::{ObserverEvent, ObserverFamily, ObserverPhase};

fn families(events: &[ObserverEvent]) -> Vec<ObserverFamily> {
    events
        .iter()
        .filter_map(|event| match event {
            ObserverEvent::Message(event)
            | ObserverEvent::ContentBlock(event)
            | ObserverEvent::Tool(event)
            | ObserverEvent::UserInputRequest(event)
            | ObserverEvent::Plan(event)
            | ObserverEvent::Task(event)
            | ObserverEvent::NativeMarker(event)
            | ObserverEvent::EffectiveState(event)
            | ObserverEvent::ActorRun(event)
            | ObserverEvent::ActorAffiliation(event)
            | ObserverEvent::UsageV2(event) => Some(event.family),
            _ => None,
        })
        .collect()
}

fn resets(events: &[ObserverEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ObserverEvent::Reset(event) => Some(event.reason.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn bootstrap_delivers_typed_revisions_from_a_real_transcript() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[user_record("u-1"), assistant_record("a-1", "resp-1", 10)],
    );

    let observer = fixture.open();
    let events = drain_bootstrap(&observer);

    // The Claude adapter emits actor-run and usage-v2 from a transcript today.
    // The other nine families are carried by the same wire but have no producer
    // yet; see `families.rs`.
    let seen = families(&events);
    assert!(
        seen.contains(&ObserverFamily::ActorRun),
        "bootstrap delivered no actor-run family: {seen:?}"
    );
    assert!(
        seen.contains(&ObserverFamily::UsageV2),
        "bootstrap delivered no usage-v2 family: {seen:?}"
    );

    let usage = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::UsageV2(event) => Some(event),
            _ => None,
        })
        .expect("usage event");
    assert_eq!(usage.scope_epoch, 1, "bootstrap is always epoch 1");
    assert!(
        usage.value.is_some(),
        "an upsert carries its reduced typed value"
    );
    assert!(
        usage.source.byte_end > usage.source.byte_start,
        "an append-delimited event names its byte range"
    );
    assert!(
        !usage.source.record_digest.is_empty(),
        "an event names the digest of the record behind it"
    );
    assert_eq!(
        usage.actor.native_session_id,
        super::support::SESSION,
        "every event carries root identity"
    );

    let barrier = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::BootstrapComplete(barrier) => Some(barrier),
            _ => None,
        })
        .expect("bootstrap barrier");
    assert!(barrier.root_present, "the root transcript exists");
    assert!(
        resets(&events).is_empty(),
        "a cold read of an unchanged file must not fabricate a reset"
    );
    observer.close();
}

#[test]
fn live_append_after_bootstrap_is_delivered() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &[user_record("u-1")]);

    let observer = fixture.open();
    let bootstrap = drain_bootstrap(&observer);
    assert!(!semantic_ids(&bootstrap).is_empty());

    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-live", "resp-live", 7)],
    );
    let live = collect_until(&observer, Duration::from_secs(10), |events| {
        families(events).contains(&ObserverFamily::UsageV2)
    });
    assert!(
        families(&live).contains(&ObserverFamily::UsageV2),
        "a live append delivered no usage revision"
    );
    observer.close();
}

#[test]
fn a_partial_trailing_line_is_buffered_until_it_completes() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &[user_record("u-1")]);
    let observer = fixture.open();
    let _bootstrap = drain_bootstrap(&observer);

    let record = assistant_record("a-partial", "resp-partial", 11);
    let split = record.len() / 2;
    fixture.append_partial(&fixture.transcript(), &record[..split]);

    // Give the observer several reconciliation passes at the half record.
    let during = collect_until(&observer, Duration::from_millis(300), |_| false);
    assert!(
        !families(&during).contains(&ObserverFamily::UsageV2),
        "a half-written record must not decode"
    );
    assert!(
        resets(&during).is_empty(),
        "a partial suffix is not a discontinuity"
    );

    fixture.append_partial(&fixture.transcript(), &record[split..]);
    fixture.append_partial(&fixture.transcript(), "\n");
    let after = collect_until(&observer, Duration::from_secs(10), |events| {
        families(events).contains(&ObserverFamily::UsageV2)
    });
    assert!(
        families(&after).contains(&ObserverFamily::UsageV2),
        "the completed record was never delivered"
    );
    observer.close();
}

#[test]
fn truncating_the_transcript_resets_before_replaying_it() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[
            assistant_record("a-1", "resp-1", 10),
            assistant_record("a-2", "resp-2", 20),
        ],
    );
    let observer = fixture.open();
    let _bootstrap = drain_bootstrap(&observer);

    let first_line = assistant_record("a-1", "resp-1", 10).len() as u64 + 1;
    fixture.truncate(&fixture.transcript(), first_line);

    let events = collect_until(&observer, Duration::from_secs(10), |events| {
        !resets(events).is_empty()
    });
    assert_eq!(
        resets(&events),
        vec!["truncated".to_string()],
        "truncation must be reported as a generation reset"
    );
    let reset_at = events
        .iter()
        .position(|event| matches!(event, ObserverEvent::Reset(_)))
        .expect("reset control");
    assert!(
        events[reset_at..]
            .iter()
            .filter(|event| !event.is_control())
            .all(|event| matches!(
                event,
                ObserverEvent::UsageV2(_)
                    | ObserverEvent::ActorRun(_)
                    | ObserverEvent::ActorAffiliation(_)
            )),
        "replay after a reset must follow the reset control"
    );
    observer.close();
}

#[test]
fn rewriting_the_transcript_wholesale_reports_one_discontinuity() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 10)],
    );
    let observer = fixture.open();
    let _bootstrap = drain_bootstrap(&observer);

    fixture.rewrite(
        &fixture.transcript(),
        &[
            assistant_record("b-1", "resp-b1", 30),
            assistant_record("b-2", "resp-b2", 40),
        ],
    );

    let events = collect_until(&observer, Duration::from_secs(10), |events| {
        !resets(events).is_empty()
            && events
                .iter()
                .any(|event| matches!(event, ObserverEvent::UsageV2(_)))
    });
    assert_eq!(resets(&events).len(), 1, "one rewrite is one discontinuity");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_))),
        "the rewritten content was never replayed"
    );
    observer.close();
}

#[test]
fn replacing_the_transcript_file_is_a_discontinuity_not_an_append() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 10)],
    );
    let observer = fixture.open();
    let _bootstrap = drain_bootstrap(&observer);

    // Rotate: write a replacement beside the original and move it into place,
    // which changes the file's native identity under the same path.
    let rotated = fixture.transcript().with_extension("jsonl.rotated");
    std::fs::write(
        &rotated,
        format!("{}\n", assistant_record("c-1", "resp-c1", 50)),
    )
    .expect("write rotation");
    std::fs::rename(&rotated, fixture.transcript()).expect("rotate into place");

    let events = collect_until(&observer, Duration::from_secs(10), |events| {
        !resets(events).is_empty()
    });
    assert!(
        !resets(&events).is_empty(),
        "a replaced file must not be read as a continued append"
    );
    observer.close();
}

#[test]
fn a_repeated_usage_row_adds_nothing_and_a_correction_replaces_it() {
    let fixture = SessionFixture::new();
    let repeated = assistant_record("a-1", "resp-1", 100);
    fixture.append(&fixture.transcript(), &[repeated.clone()]);

    let observer = fixture.open();
    let bootstrap = drain_bootstrap(&observer);
    let first: Vec<_> = bootstrap
        .iter()
        .filter_map(|event| match event {
            ObserverEvent::UsageV2(event) => Some(event.fact_id),
            _ => None,
        })
        .collect();
    assert_eq!(first.len(), 1, "one response is one usage revision");

    // The same response reported again adds nothing: usage-v2 replaces.
    fixture.append(&fixture.transcript(), &[repeated]);
    let repeat_window = collect_until(&observer, Duration::from_millis(400), |_| false);
    assert!(
        !repeat_window
            .iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_))),
        "an exact repeat of a usage revision must be suppressed before the wire"
    );

    // A downward correction for the same response is a new revision.
    fixture.append(
        &fixture.transcript(),
        &[assistant_record("a-1", "resp-1", 40)],
    );
    let corrected = collect_until(&observer, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::UsageV2(_)))
    });
    let correction = corrected
        .iter()
        .find_map(|event| match event {
            ObserverEvent::UsageV2(event) => Some(event),
            _ => None,
        })
        .expect("a downward correction is still a revision");
    assert_eq!(
        correction.fact_id, first[0],
        "a correction replaces the same response's usage rather than adding one"
    );
    observer.close();
}

#[test]
fn a_record_the_decoder_cannot_interpret_does_not_terminate_the_stream() {
    let fixture = SessionFixture::new();
    fixture.append(
        &fixture.transcript(),
        &[
            assistant_record("a-1", "resp-1", 10),
            // A shape no Claude decoder claims.
            r#"{"type":"a-kind-from-the-future","uuid":"x-1","sessionId":"01234567-89ab-cdef-0123-456789abcdef","payload":{"unmapped":true}}"#.to_string(),
            assistant_record("a-2", "resp-2", 20),
        ],
    );

    let observer = fixture.open();
    let events = drain_bootstrap(&observer);

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObserverEvent::UsageV2(_)))
            .count(),
        2,
        "records on both sides of an uninterpretable line must still decode"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::BootstrapComplete(_))),
        "one uninterpretable line must not stop bootstrap from completing"
    );
    observer.close();
}

#[test]
fn a_session_larger_than_the_pass_bound_still_completes_bootstrap_first() {
    // The append driver frames at most 1,024 records per read, so a scope with
    // more than 1,024 x 64 records used to exhaust the per-wake pass bound and
    // publish `bootstrap_complete` over a truncated manifest, delivering the
    // rest as `Live` — cold state arriving labelled as new activity.
    let fixture = SessionFixture::new();
    // Each record reduces to its own usage revision, so a truncated bootstrap
    // leaves real revisions behind rather than empty lines.
    let records: Vec<String> = (0..66_000).map(compact_assistant_record).collect();
    fixture.append_once(&fixture.transcript(), &records);

    let observer = fixture.open();
    // Decoding this many records takes about ten seconds in a debug build, so
    // the bound is generous on purpose: it exists to give the work room, not to
    // assert anything about timing.
    let mut events = drain_bootstrap_within(&observer, Duration::from_secs(120));
    let barrier_at = events
        .iter()
        .position(|event| matches!(event, ObserverEvent::BootstrapComplete(_)))
        .expect("bootstrap barrier");

    // Keep draining past the barrier. Nothing was appended, so anything that
    // arrives now was already on disk at attach and should have been delivered
    // before the barrier claimed complete coverage.
    events.extend(collect_until(&observer, Duration::from_secs(3), |_| false));

    let stragglers: Vec<ObserverPhase> = events[barrier_at + 1..]
        .iter()
        .filter_map(|event| match event {
            ObserverEvent::Message(event)
            | ObserverEvent::ContentBlock(event)
            | ObserverEvent::Tool(event)
            | ObserverEvent::UserInputRequest(event)
            | ObserverEvent::Plan(event)
            | ObserverEvent::Task(event)
            | ObserverEvent::NativeMarker(event)
            | ObserverEvent::EffectiveState(event)
            | ObserverEvent::ActorRun(event)
            | ObserverEvent::ActorAffiliation(event)
            | ObserverEvent::UsageV2(event) => Some(event.phase),
            _ => None,
        })
        .collect();
    assert!(
        stragglers.is_empty(),
        "{} revisions that were on disk at attach arrived after the barrier \
         (phases {:?}) — the barrier claimed coverage it did not have",
        stragglers.len(),
        stragglers.iter().take(3).collect::<Vec<_>>()
    );
    observer.close();
}
