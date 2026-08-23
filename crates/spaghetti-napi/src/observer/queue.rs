//! Bounded delivery with a control lane that semantic backlog cannot starve.
//!
//! RFC 012D section 12 makes the two lanes internal capacity domains, not two
//! public streams: they are multiplexed into one ordered sequence. The rule
//! that matters is that a saturated semantic backlog can never delay a
//! mandatory lifecycle control, and that losing continuity is explicit rather
//! than silent.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use super::event::ObserverEvent;
use super::request::QueueLimits;

/// The control lane is small and separate. It is sized for the lifecycle
/// controls that can legitimately coincide, not for throughput.
const CONTROL_LANE_CAPACITY: usize = 256;

/// Outcome of offering one semantic event to the bounded queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    Accepted,
    /// The queue is full. During bootstrap and correction the producer waits;
    /// during live delivery this becomes explicit continuity loss.
    Full,
}

struct Lanes {
    epoch: u64,
    next_sequence: u64,
    semantic: VecDeque<ObserverEvent>,
    control: VecDeque<ObserverEvent>,
    retained_bytes: usize,
    limits: QueueLimits,
    /// False between continuity loss and the completion of the replacement
    /// epoch. Ordinary semantic delivery stops while it is false.
    epoch_valid: bool,
    last_contiguous_sequence: u64,
    closing: bool,
    closed: bool,
    discarded: u32,
    terminal_error: Option<String>,
}

/// The shared delivery state between the owner thread and the consumer.
pub(crate) struct Delivery {
    lanes: Mutex<Lanes>,
    signal: Condvar,
}

/// A snapshot of delivery state for `status()`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeliveryStatus {
    pub epoch: u64,
    pub offered_through_sequence: u64,
    pub queued_semantic: u32,
    pub queued_control: u32,
    pub retained_bytes: u32,
    pub epoch_valid: bool,
    pub closed: bool,
}

impl Delivery {
    pub(crate) fn new(limits: QueueLimits) -> Self {
        Self {
            lanes: Mutex::new(Lanes {
                epoch: 1,
                next_sequence: 1,
                semantic: VecDeque::new(),
                control: VecDeque::new(),
                retained_bytes: 0,
                limits,
                epoch_valid: true,
                last_contiguous_sequence: 0,
                closing: false,
                closed: false,
                discarded: 0,
                terminal_error: None,
            }),
            signal: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Lanes> {
        self.lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.lock().closing
    }

    /// Offer one semantic event. The caller decides what a `Full` result means
    /// for the current delivery phase.
    pub(crate) fn admit_semantic(&self, mut event: ObserverEvent) -> Admission {
        debug_assert!(!event.is_control(), "control events take the control lane");
        let mut lanes = self.lock();
        if lanes.closing || !lanes.epoch_valid {
            return Admission::Accepted;
        }
        let bytes = event.retained_bytes();
        if lanes.semantic.len() >= lanes.limits.max_events
            || lanes.retained_bytes.saturating_add(bytes) > lanes.limits.max_bytes
        {
            return Admission::Full;
        }
        let sequence = lanes.next_sequence;
        lanes.next_sequence += 1;
        lanes.last_contiguous_sequence = sequence;
        event.set_sequence(sequence);
        lanes.retained_bytes += bytes;
        lanes.semantic.push_back(event);
        self.signal.notify_all();
        Admission::Accepted
    }

    /// Offer one semantic event, waiting for room. Used during bootstrap and
    /// correction, where queue fullness must apply producer backpressure
    /// instead of manufacturing continuity loss.
    pub(crate) fn admit_semantic_blocking(&self, event: ObserverEvent) -> Admission {
        loop {
            match self.admit_semantic(event.clone()) {
                Admission::Accepted => return Admission::Accepted,
                Admission::Full => {
                    let lanes = self.lock();
                    if lanes.closing {
                        return Admission::Accepted;
                    }
                    let _unused = self
                        .signal
                        .wait_timeout(lanes, Duration::from_millis(50))
                        .map(|(guard, _)| guard);
                }
            }
        }
    }

    /// Lifecycle and continuity controls. They never compete with semantic
    /// events for capacity; if the control lane itself cannot hold a mandatory
    /// state, the observer becomes terminally failed rather than losing it.
    pub(crate) fn admit_control(&self, mut event: ObserverEvent) {
        debug_assert!(event.is_control(), "semantic events take the bounded lane");
        let mut lanes = self.lock();
        if lanes.control.len() >= CONTROL_LANE_CAPACITY {
            lanes.terminal_error =
                Some("observer control lane overflowed; continuity is not claimed".to_string());
            return;
        }
        let sequence = lanes.next_sequence;
        lanes.next_sequence += 1;
        event.set_sequence(sequence);
        lanes.control.push_back(event);
        self.signal.notify_all();
    }

    /// Continuity is lost. Every not-yet-delivered ordinary event in this epoch
    /// is discarded: the replacement snapshot restores current state, so
    /// keeping partial events would only invite a partial merge.
    pub(crate) fn invalidate_epoch(&self) -> (u64, u64, u32) {
        let mut lanes = self.lock();
        let discarded = lanes.semantic.len() as u32;
        lanes.semantic.clear();
        lanes.retained_bytes = 0;
        lanes.discarded = lanes.discarded.saturating_add(discarded);
        lanes.epoch_valid = false;
        (lanes.epoch, lanes.last_contiguous_sequence, discarded)
    }

    /// Open the replacement epoch. Deduplication is epoch-scoped, so a stable
    /// event id may legitimately reappear here with fresh context.
    pub(crate) fn begin_replacement_epoch(&self) -> u64 {
        let mut lanes = self.lock();
        lanes.epoch += 1;
        lanes.epoch_valid = true;
        lanes.epoch
    }

    /// Take up to `max` events in sequence order across both lanes.
    pub(crate) fn drain(&self, max: usize) -> Vec<ObserverEvent> {
        let mut lanes = self.lock();
        let mut taken = Vec::new();
        while taken.len() < max {
            let next_control = lanes.control.front().map(ObserverEvent::sequence);
            let next_semantic = lanes.semantic.front().map(ObserverEvent::sequence);
            let take_control = match (next_control, next_semantic) {
                (None, None) => break,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(control), Some(semantic)) => control <= semantic,
            };
            let event = if take_control {
                lanes.control.pop_front()
            } else {
                lanes.semantic.pop_front().inspect(|event| {
                    lanes.retained_bytes =
                        lanes.retained_bytes.saturating_sub(event.retained_bytes());
                })
            };
            let Some(event) = event else { break };
            taken.push(event);
        }
        if !taken.is_empty() {
            self.signal.notify_all();
        }
        taken
    }

    /// Block until at least one event is available, the observer closes, or the
    /// timeout expires.
    pub(crate) fn wait_for_events(&self, timeout: Duration, max: usize) -> Vec<ObserverEvent> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            {
                let lanes = self.lock();
                if !lanes.control.is_empty() || !lanes.semantic.is_empty() || lanes.closed {
                    drop(lanes);
                    return self.drain(max);
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Vec::new();
                }
                let _unused = self
                    .signal
                    .wait_timeout(lanes, remaining)
                    .map(|(guard, _)| guard);
            }
            if std::time::Instant::now() >= deadline {
                return self.drain(max);
            }
        }
    }

    /// Request close. Cancels new work and wakes every waiter.
    pub(crate) fn request_close(&self) {
        let mut lanes = self.lock();
        lanes.closing = true;
        self.signal.notify_all();
    }

    /// Mark the observer finished after the owner thread has stopped.
    pub(crate) fn mark_closed(&self) -> u32 {
        let mut lanes = self.lock();
        lanes.closed = true;
        let discarded = lanes.discarded.saturating_add(lanes.semantic.len() as u32);
        lanes.semantic.clear();
        lanes.retained_bytes = 0;
        self.signal.notify_all();
        discarded
    }

    pub(crate) fn take_terminal_error(&self) -> Option<String> {
        self.lock().terminal_error.take()
    }

    pub(crate) fn status(&self) -> DeliveryStatus {
        let lanes = self.lock();
        DeliveryStatus {
            epoch: lanes.epoch,
            offered_through_sequence: lanes.next_sequence.saturating_sub(1),
            queued_semantic: lanes.semantic.len() as u32,
            queued_control: lanes.control.len() as u32,
            retained_bytes: u32::try_from(lanes.retained_bytes).unwrap_or(u32::MAX),
            epoch_valid: lanes.epoch_valid,
            closed: lanes.closed,
        }
    }
}
