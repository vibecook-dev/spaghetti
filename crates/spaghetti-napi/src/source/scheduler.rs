use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use super::{DirtyReason, SourceDriverError};

const FAIR_SEQUENCE: [IngestPriority; 8] = [
    IngestPriority::Interactive,
    IngestPriority::Interactive,
    IngestPriority::Interactive,
    IngestPriority::Interactive,
    IngestPriority::ForegroundRepair,
    IngestPriority::ForegroundRepair,
    IngestPriority::Backfill,
    IngestPriority::Maintenance,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SharedSourcePassPoolError {
    #[error("shared source pass capacity is outside the supported bound")]
    InvalidCapacity,
}

/// Caller-owned fair permit domain for bounded source access/decode passes.
/// Durable, catalog, and scoped runtimes may share this authority without any
/// one workload being able to resize or replace it after construction.
#[derive(Clone)]
pub(crate) struct SharedSourcePassPool {
    inner: Arc<SharedSourcePassPoolInner>,
}

struct SharedSourcePassPoolInner {
    state: Mutex<SharedSourcePassPoolState>,
    max_concurrent_passes: usize,
}

struct SharedSourcePassPoolState {
    active: usize,
    concurrency_limit: usize,
    queues: [VecDeque<Arc<SharedSourcePassWaiter>>; 4],
    fairness_cursor: usize,
    next_waiter_id: u64,
}

struct SharedSourcePassWaiter {
    id: u64,
    granted: AtomicBool,
    notified: tokio::sync::Notify,
}

pub(crate) struct SharedSourcePassPermit {
    inner: Arc<SharedSourcePassPoolInner>,
    released: bool,
}

struct SharedSourcePassRegistration {
    inner: Arc<SharedSourcePassPoolInner>,
    waiter: Arc<SharedSourcePassWaiter>,
    active: bool,
}

impl SharedSourcePassPool {
    pub(crate) fn new(max_concurrent_passes: usize) -> Result<Self, SharedSourcePassPoolError> {
        if max_concurrent_passes == 0 || max_concurrent_passes > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(SharedSourcePassPoolError::InvalidCapacity);
        }
        Ok(Self {
            inner: Arc::new(SharedSourcePassPoolInner {
                state: Mutex::new(SharedSourcePassPoolState {
                    active: 0,
                    concurrency_limit: max_concurrent_passes,
                    queues: std::array::from_fn(|_| VecDeque::new()),
                    fairness_cursor: 0,
                    next_waiter_id: 1,
                }),
                max_concurrent_passes,
            }),
        })
    }

    pub(crate) fn max_concurrent_passes(&self) -> usize {
        self.inner.max_concurrent_passes
    }

    pub(crate) fn set_concurrency_limit(
        &self,
        concurrency_limit: usize,
    ) -> Result<(), SharedSourcePassPoolError> {
        if concurrency_limit == 0 || concurrency_limit > self.inner.max_concurrent_passes {
            return Err(SharedSourcePassPoolError::InvalidCapacity);
        }
        let notifications = {
            let mut state = self.lock_state();
            state.concurrency_limit = concurrency_limit;
            grant_shared_source_waiters(&mut state)
        };
        notify_shared_source_waiters(notifications);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn concurrency_limit(&self) -> usize {
        self.lock_state().concurrency_limit
    }

    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        let state = self.lock_state();
        state.concurrency_limit.saturating_sub(state.active)
    }

    #[cfg(test)]
    pub(crate) fn queued_waiters(&self) -> usize {
        self.lock_state().queues.iter().map(VecDeque::len).sum()
    }

    #[cfg(test)]
    pub(crate) async fn acquire(&self) -> SharedSourcePassPermit {
        self.acquire_priority(IngestPriority::Interactive).await
    }

    pub(crate) async fn acquire_priority(
        &self,
        priority: IngestPriority,
    ) -> SharedSourcePassPermit {
        self.register(priority).wait().await
    }

    /// Blocking acquire for catalog/query worker threads that are not on a
    /// Tokio runtime. Observer source owners keep using [`Self::acquire`].
    pub(crate) fn blocking_acquire(&self) -> SharedSourcePassPermit {
        self.blocking_acquire_priority(IngestPriority::Interactive)
    }

    pub(crate) fn blocking_acquire_priority(
        &self,
        priority: IngestPriority,
    ) -> SharedSourcePassPermit {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("shared pass pool blocking runtime")
            .block_on(self.acquire_priority(priority))
    }

    fn register(&self, priority: IngestPriority) -> SharedSourcePassRegistration {
        let (waiter, notifications) = {
            let mut state = self.lock_state();
            let waiter = Arc::new(SharedSourcePassWaiter {
                id: state.next_waiter_id,
                granted: AtomicBool::new(false),
                notified: tokio::sync::Notify::new(),
            });
            state.next_waiter_id = state.next_waiter_id.wrapping_add(1).max(1);
            state.queues[priority.index()].push_back(Arc::clone(&waiter));
            let notifications = grant_shared_source_waiters(&mut state);
            (waiter, notifications)
        };
        notify_shared_source_waiters(notifications);
        SharedSourcePassRegistration {
            inner: Arc::clone(&self.inner),
            waiter,
            active: true,
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SharedSourcePassPoolState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SharedSourcePassRegistration {
    async fn wait(mut self) -> SharedSourcePassPermit {
        loop {
            let notified = self.waiter.notified.notified();
            if self.waiter.granted.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        self.active = false;
        SharedSourcePassPermit {
            inner: Arc::clone(&self.inner),
            released: false,
        }
    }
}

impl Drop for SharedSourcePassRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let notifications = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if self.waiter.granted.load(Ordering::Acquire) {
                state.active = state.active.saturating_sub(1);
            } else {
                for queue in &mut state.queues {
                    if let Some(index) = queue.iter().position(|waiter| waiter.id == self.waiter.id)
                    {
                        queue.remove(index);
                        break;
                    }
                }
            }
            grant_shared_source_waiters(&mut state)
        };
        notify_shared_source_waiters(notifications);
    }
}

impl Drop for SharedSourcePassPermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let notifications = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.active = state.active.saturating_sub(1);
            grant_shared_source_waiters(&mut state)
        };
        notify_shared_source_waiters(notifications);
    }
}

fn grant_shared_source_waiters(
    state: &mut SharedSourcePassPoolState,
) -> Vec<Arc<SharedSourcePassWaiter>> {
    let mut notifications = Vec::new();
    while state.active < state.concurrency_limit {
        let mut selected = None;
        for _ in 0..FAIR_SEQUENCE.len() {
            let priority = FAIR_SEQUENCE[state.fairness_cursor];
            state.fairness_cursor = (state.fairness_cursor + 1) % FAIR_SEQUENCE.len();
            if let Some(waiter) = state.queues[priority.index()].pop_front() {
                selected = Some(waiter);
                break;
            }
        }
        let Some(waiter) = selected else {
            break;
        };
        state.active = state.active.saturating_add(1);
        waiter.granted.store(true, Ordering::Release);
        notifications.push(waiter);
    }
    notifications
}

fn notify_shared_source_waiters(waiters: Vec<Arc<SharedSourcePassWaiter>>) {
    for waiter in waiters {
        waiter.notified.notify_one();
    }
}

impl std::fmt::Debug for SharedSourcePassPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock_state();
        formatter
            .debug_struct("SharedSourcePassPool")
            .field("max_concurrent_passes", &self.inner.max_concurrent_passes)
            .field("concurrency_limit", &state.concurrency_limit)
            .field("active", &state.active)
            .field(
                "queued",
                &state.queues.iter().map(VecDeque::len).sum::<usize>(),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IngestPriority {
    Interactive,
    ForegroundRepair,
    Backfill,
    Maintenance,
}

impl IngestPriority {
    const fn index(self) -> usize {
        match self {
            Self::Interactive => 0,
            Self::ForegroundRepair => 1,
            Self::Backfill => 2,
            Self::Maintenance => 3,
        }
    }

    const fn outranks(self, other: Self) -> bool {
        self.index() < other.index()
    }
}

/// Serialization key for source read/decode work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkKey {
    pub object_key: Vec<u8>,
    pub generation: u64,
}

impl WorkKey {
    pub fn new(object_key: Vec<u8>, generation: u64) -> Result<Self, SourceDriverError> {
        if object_key.is_empty() {
            return Err(SourceDriverError::InvalidConfig(
                "scheduler object key must not be empty".to_string(),
            ));
        }
        if generation == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "scheduler generation must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            object_key,
            generation,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledWork {
    pub key: WorkKey,
    pub priority: IngestPriority,
    pub reason: DirtyReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleOutcome {
    Enqueued,
    Coalesced,
    PriorityEscalated,
    FullNeedsReconcile,
}

/// A bounded, weighted-fair scheduler for the parallel source read/decode
/// lane. Projection commits remain on the separate single writer lane.
pub struct BoundedScheduler {
    capacity: usize,
    max_in_flight: usize,
    queues: [VecDeque<ScheduledWork>; 4],
    in_flight: HashSet<WorkKey>,
    fairness_cursor: usize,
    recovery_required: bool,
}

impl BoundedScheduler {
    pub fn new(capacity: usize, max_in_flight: usize) -> Result<Self, SourceDriverError> {
        if capacity == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "scheduler capacity must be greater than zero".to_string(),
            ));
        }
        if max_in_flight == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "scheduler max_in_flight must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            capacity,
            max_in_flight,
            queues: std::array::from_fn(|_| VecDeque::new()),
            in_flight: HashSet::with_capacity(max_in_flight),
            fairness_cursor: 0,
            recovery_required: false,
        })
    }

    pub fn enqueue(&mut self, work: ScheduledWork) -> ScheduleOutcome {
        if let Some((queue_index, entry_index)) = self.find_queued(&work.key) {
            let current = &mut self.queues[queue_index][entry_index];
            current.reason = current.reason.merge(work.reason);
            if work.priority.outranks(current.priority) {
                let mut promoted = self.queues[queue_index]
                    .remove(entry_index)
                    .expect("located scheduler entry must still exist");
                promoted.priority = work.priority;
                self.queues[promoted.priority.index()].push_back(promoted);
                return ScheduleOutcome::PriorityEscalated;
            }
            return ScheduleOutcome::Coalesced;
        }

        if self.queued_len() >= self.capacity {
            // The caller must collapse the affected scope to a reconcile hint.
            // Remembering the condition prevents a queue-full result from ever
            // being interpreted as a safely dropped event.
            self.recovery_required = true;
            return ScheduleOutcome::FullNeedsReconcile;
        }
        self.queues[work.priority.index()].push_back(work);
        ScheduleOutcome::Enqueued
    }

    pub fn dispatch(&mut self) -> Option<ScheduledWork> {
        if self.in_flight.len() >= self.max_in_flight {
            return None;
        }

        for _ in 0..FAIR_SEQUENCE.len() {
            let priority = FAIR_SEQUENCE[self.fairness_cursor];
            self.fairness_cursor = (self.fairness_cursor + 1) % FAIR_SEQUENCE.len();
            if let Some(work) = self.take_ready(priority) {
                self.in_flight.insert(work.key.clone());
                return Some(work);
            }
        }
        None
    }

    /// Frees the serial lane for this object/generation. Returns false for a
    /// duplicate or unknown completion so hosts can report invariant breaks.
    pub fn complete(&mut self, key: &WorkKey) -> bool {
        self.in_flight.remove(key)
    }

    pub fn queued_len(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }

    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }

    pub fn acknowledge_recovery(&mut self) {
        self.recovery_required = false;
    }

    pub fn cancel_generation(&mut self, key: &WorkKey) -> usize {
        let mut removed = 0;
        for queue in &mut self.queues {
            let before = queue.len();
            queue.retain(|work| &work.key != key);
            removed += before - queue.len();
        }
        removed
    }

    fn find_queued(&self, key: &WorkKey) -> Option<(usize, usize)> {
        self.queues
            .iter()
            .enumerate()
            .find_map(|(queue_index, queue)| {
                queue
                    .iter()
                    .position(|work| &work.key == key)
                    .map(|entry_index| (queue_index, entry_index))
            })
    }

    fn take_ready(&mut self, priority: IngestPriority) -> Option<ScheduledWork> {
        let queue = &mut self.queues[priority.index()];
        let candidates = queue.len();
        for _ in 0..candidates {
            let work = queue
                .pop_front()
                .expect("scheduler queue length was captured before pop");
            if self.in_flight.contains(&work.key) {
                queue.push_back(work);
            } else {
                return Some(work);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(name: u8, priority: IngestPriority) -> ScheduledWork {
        ScheduledWork {
            key: WorkKey::new(vec![name], 1).unwrap(),
            priority,
            reason: DirtyReason::NativeEvent,
        }
    }

    #[test]
    fn queue_is_bounded_and_overflow_requires_reconcile() {
        let mut scheduler = BoundedScheduler::new(2, 1).unwrap();
        assert_eq!(
            scheduler.enqueue(work(1, IngestPriority::Backfill)),
            ScheduleOutcome::Enqueued
        );
        assert_eq!(
            scheduler.enqueue(work(2, IngestPriority::Backfill)),
            ScheduleOutcome::Enqueued
        );
        assert_eq!(
            scheduler.enqueue(work(3, IngestPriority::Interactive)),
            ScheduleOutcome::FullNeedsReconcile
        );
        assert_eq!(scheduler.queued_len(), 2);
        assert!(scheduler.recovery_required());
    }

    #[test]
    fn duplicate_work_coalesces_and_escalates_priority() {
        let mut scheduler = BoundedScheduler::new(4, 1).unwrap();
        assert_eq!(
            scheduler.enqueue(work(1, IngestPriority::Maintenance)),
            ScheduleOutcome::Enqueued
        );
        assert_eq!(
            scheduler.enqueue(work(1, IngestPriority::Maintenance)),
            ScheduleOutcome::Coalesced
        );
        assert_eq!(
            scheduler.enqueue(work(1, IngestPriority::Interactive)),
            ScheduleOutcome::PriorityEscalated
        );
        assert_eq!(scheduler.queued_len(), 1);
        assert_eq!(
            scheduler.dispatch().unwrap().priority,
            IngestPriority::Interactive
        );
    }

    #[test]
    fn same_object_and_generation_is_serial() {
        let mut scheduler = BoundedScheduler::new(4, 2).unwrap();
        let first = work(1, IngestPriority::Interactive);
        scheduler.enqueue(first.clone());
        let running = scheduler.dispatch().unwrap();
        scheduler.enqueue(first);
        scheduler.enqueue(work(2, IngestPriority::Interactive));

        assert_eq!(scheduler.dispatch().unwrap().key.object_key, vec![2]);
        assert_eq!(scheduler.in_flight_len(), 2);
        assert!(scheduler.dispatch().is_none());
        assert!(scheduler.complete(&running.key));
        assert_eq!(scheduler.dispatch().unwrap().key, running.key);
    }

    #[test]
    fn continuously_refilled_interactive_work_does_not_starve_maintenance() {
        let mut scheduler = BoundedScheduler::new(32, 1).unwrap();
        scheduler.enqueue(work(200, IngestPriority::Maintenance));
        for id in 0..16 {
            scheduler.enqueue(work(id, IngestPriority::Interactive));
        }

        let mut maintenance_dispatch = None;
        for dispatch_index in 0..8 {
            let next = scheduler.dispatch().unwrap();
            if next.priority == IngestPriority::Maintenance {
                maintenance_dispatch = Some(dispatch_index);
            }
            assert!(scheduler.complete(&next.key));
            scheduler.enqueue(work(100 + dispatch_index, IngestPriority::Interactive));
        }
        assert!(maintenance_dispatch.is_some_and(|index| index <= 7));
    }

    #[tokio::test]
    async fn shared_pass_pool_weights_priorities_without_starving_maintenance() {
        let pool = SharedSourcePassPool::new(1).unwrap();
        let held = pool.acquire_priority(IngestPriority::Interactive).await;
        let registrations = [
            IngestPriority::Interactive,
            IngestPriority::Interactive,
            IngestPriority::Interactive,
            IngestPriority::Interactive,
            IngestPriority::Interactive,
            IngestPriority::Interactive,
            IngestPriority::Interactive,
            IngestPriority::Interactive,
            IngestPriority::Maintenance,
        ]
        .into_iter()
        .map(|priority| (priority, pool.register(priority)))
        .collect::<Vec<_>>();
        assert_eq!(pool.queued_waiters(), registrations.len());

        let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();
        let tasks = registrations
            .into_iter()
            .map(|(priority, registration)| {
                let order_tx = order_tx.clone();
                tokio::spawn(async move {
                    let _permit = registration.wait().await;
                    order_tx.send(priority).unwrap();
                })
            })
            .collect::<Vec<_>>();
        drop(order_tx);
        drop(held);

        let mut order = Vec::new();
        while let Some(priority) = order_rx.recv().await {
            order.push(priority);
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(order.len(), 9);
        assert!(
            order
                .iter()
                .position(|priority| *priority == IngestPriority::Maintenance)
                .is_some_and(|index| index <= 4),
            "maintenance must receive a bounded turn: {order:?}"
        );
    }

    #[tokio::test]
    async fn shared_pass_pool_cancels_waiters_and_applies_dynamic_limits_safely() {
        let pool = SharedSourcePassPool::new(2).unwrap();
        let first = pool.acquire().await;
        let second = pool.acquire().await;
        pool.set_concurrency_limit(1).unwrap();
        assert_eq!(pool.concurrency_limit(), 1);
        assert_eq!(pool.available_permits(), 0);

        let registration = pool.register(IngestPriority::Maintenance);
        assert_eq!(pool.queued_waiters(), 1);
        drop(registration);
        assert_eq!(pool.queued_waiters(), 0);

        let waiting = pool.register(IngestPriority::Backfill);
        drop(first);
        assert_eq!(pool.available_permits(), 0);
        assert_eq!(pool.queued_waiters(), 1);
        drop(second);
        let permit = waiting.wait().await;
        assert_eq!(pool.queued_waiters(), 0);
        drop(permit);
        assert_eq!(pool.available_permits(), 1);

        pool.set_concurrency_limit(2).unwrap();
        assert_eq!(pool.available_permits(), 2);
        assert!(pool.set_concurrency_limit(0).is_err());
        assert!(pool.set_concurrency_limit(3).is_err());
    }
}
