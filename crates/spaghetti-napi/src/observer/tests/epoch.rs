//! Continuity: overflow, replacement epochs, and close.

use std::time::Duration;

use super::support::{
    assistant_record, barrier_manifest, collect_until, drain_bootstrap, semantic_ids,
    SessionFixture,
};
use crate::observer::{ObserverEvent, ObserverHandle};

/// Enough records that a two-event queue cannot hold the live batch.
fn burst(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| assistant_record(&format!("a-{index}"), &format!("resp-{index}"), 10))
        .collect()
}

#[test]
fn a_saturated_live_queue_reports_continuity_loss_instead_of_dropping_events() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &burst(4));

    // A queue this small cannot absorb the live burst below, which is the
    // point: a live producer must not stall on a slow consumer, so continuity
    // loss has to be explicit.
    let mut request = fixture.request();
    request.max_queued_events = Some(2);
    let observer = ObserverHandle::open(&request).expect("observer attaches");
    let _bootstrap = drain_bootstrap(&observer);

    fixture.append(&fixture.transcript(), &burst(40)[4..].to_vec());
    std::thread::sleep(Duration::from_millis(500));

    let events = collect_until(&observer, Duration::from_secs(30), |events| {
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::ResyncComplete(_)))
    });

    let overflow = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::Overflow(event) => Some(event),
            _ => None,
        })
        .expect("a saturated live queue must report continuity loss, not drop silently");
    let resync = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::ResyncComplete(barrier) => Some(barrier),
            _ => None,
        })
        .expect("continuity loss must be followed by a completed replacement");
    assert!(
        resync.scope_epoch > overflow.scope_epoch,
        "a replacement snapshot must land in a new epoch"
    );
    assert!(
        events
            .iter()
            .position(|event| matches!(event, ObserverEvent::Overflow(_)))
            < events
                .iter()
                .position(|event| matches!(event, ObserverEvent::ResyncComplete(_))),
        "continuity loss is announced before its replacement completes"
    );
    observer.close();
}

#[test]
fn a_replacement_snapshot_equals_a_clean_bootstrap_at_the_same_coverage() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &burst(20));

    // One slot: bootstrap applies producer backpressure rather than overflowing,
    // so the observer is fully caught up before the burst below.
    let mut request = fixture.request();
    request.max_queued_events = Some(1);
    let observer = ObserverHandle::open(&request).expect("observer attaches");
    let _bootstrap = drain_bootstrap(&observer);

    // Written in one call so the observer reads the file at an exact watermark:
    // either none of these records or all of them.
    let tail: Vec<String> = (20..24)
        .map(|index| assistant_record(&format!("a-{index}"), &format!("resp-{index}"), 10))
        .collect();
    fixture.append_once(&fixture.transcript(), &tail);
    // Stop draining while the producer works: a consumer that keeps up never
    // saturates the queue, and this test is about the case where it does not.
    std::thread::sleep(Duration::from_millis(400));

    let events = collect_until(&observer, Duration::from_secs(30), |events| {
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::ResyncComplete(_)))
    });
    let resync = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::ResyncComplete(barrier) => Some(barrier),
            _ => None,
        })
        .expect("a saturated live queue must publish a replacement epoch");

    // A second observer over the same tree bootstraps cleanly. RFC 012D §13
    // requires equal per-family reduced state at equal coverage.
    let clean = fixture.open();
    let clean_bootstrap = drain_bootstrap(&clean);
    assert_eq!(
        resync
            .family_manifest
            .iter()
            .map(|entry| (
                entry.family.as_str().to_string(),
                entry.entity_count,
                entry.digest.clone()
            ))
            .collect::<Vec<_>>(),
        barrier_manifest(&clean_bootstrap),
        "resync replacement state diverged from a clean bootstrap at equal coverage"
    );
    clean.close();
    observer.close();
}

#[test]
fn close_is_idempotent_and_reports_what_it_discarded() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &burst(8));

    let mut request = fixture.request();
    request.max_queued_events = Some(2);
    let observer = ObserverHandle::open(&request).expect("observer attaches");

    // Close with work still queued and undelivered.
    observer.close();
    observer.close();

    let status = observer.status();
    assert!(status.closed, "close must settle the observer");

    let remaining = observer.poll(64);
    assert!(
        remaining
            .iter()
            .any(|event| matches!(event, ObserverEvent::Closed(_))),
        "a closed observer must deliver its close control"
    );
    assert!(
        remaining.iter().all(ObserverEvent::is_control),
        "close discards undelivered semantic events rather than replaying them"
    );
}

#[test]
fn two_attachments_to_one_tree_derive_the_same_event_ids() {
    let fixture = SessionFixture::new();
    fixture.append(&fixture.transcript(), &burst(6));

    let first = fixture.open();
    let first_events = drain_bootstrap(&first);
    first.close();

    let second = fixture.open();
    let second_events = drain_bootstrap(&second);
    second.close();

    let first_ids = semantic_ids(&first_events);
    assert!(
        !first_ids.is_empty(),
        "bootstrap delivered no semantic events"
    );
    assert_eq!(
        first_ids,
        semantic_ids(&second_events),
        "event ids must not depend on delivery sequence, wall clock, or attachment"
    );
}
