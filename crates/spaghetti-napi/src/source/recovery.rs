use std::collections::BTreeMap;

use super::SourceDriverError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DirtyScope {
    Object(Vec<u8>),
    Subtree(Vec<u8>),
    Stream(Vec<u8>),
    Instance(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DirtyReason {
    NativeEvent,
    PollDetectedChange,
    WatcherOverflow,
    InternalQueueOverflow,
    BackendError,
    CursorInvalid,
    IdentityChanged,
    RootMoved,
    Recovery,
    ManualRepair,
}

impl DirtyReason {
    pub fn is_overflow_class(self) -> bool {
        matches!(self, Self::WatcherOverflow | Self::InternalQueueOverflow)
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }

    const fn severity(self) -> u8 {
        match self {
            Self::NativeEvent => 0,
            Self::PollDetectedChange => 1,
            Self::BackendError => 2,
            Self::Recovery => 3,
            Self::CursorInvalid => 4,
            Self::IdentityChanged => 5,
            Self::RootMoved => 6,
            Self::ManualRepair => 7,
            Self::WatcherOverflow => 8,
            Self::InternalQueueOverflow => 9,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyHint {
    pub scope: DirtyScope,
    pub reason: DirtyReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintEnqueue {
    Added,
    Coalesced,
    EscalatedToInstance,
}

/// Per-instance bounded invalidation set. Capacity overflow collapses all
/// queued details into one instance reconcile rather than dropping a hint.
pub struct DirtyCoalescer {
    instance_key: Vec<u8>,
    capacity: usize,
    hints: BTreeMap<DirtyScope, DirtyReason>,
}

impl DirtyCoalescer {
    pub fn new(instance_key: Vec<u8>, capacity: usize) -> Result<Self, SourceDriverError> {
        if instance_key.is_empty() {
            return Err(SourceDriverError::InvalidConfig(
                "dirty coalescer instance key must not be empty".to_string(),
            ));
        }
        if capacity == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "dirty coalescer capacity must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            instance_key,
            capacity,
            hints: BTreeMap::new(),
        })
    }

    pub fn enqueue(&mut self, hint: DirtyHint) -> HintEnqueue {
        let instance_scope = DirtyScope::Instance(self.instance_key.clone());
        if let Some(reason) = self.hints.get_mut(&instance_scope) {
            *reason = reason.merge(hint.reason);
            return HintEnqueue::Coalesced;
        }

        if matches!(hint.scope, DirtyScope::Instance(_)) {
            let reason = self
                .hints
                .values()
                .copied()
                .fold(hint.reason, DirtyReason::merge);
            self.hints.clear();
            self.hints.insert(instance_scope, reason);
            return HintEnqueue::EscalatedToInstance;
        }

        if let Some(reason) = self.hints.get_mut(&hint.scope) {
            *reason = reason.merge(hint.reason);
            return HintEnqueue::Coalesced;
        }

        if self.hints.len() == self.capacity {
            let reason = self
                .hints
                .values()
                .copied()
                .fold(DirtyReason::InternalQueueOverflow, DirtyReason::merge);
            self.hints.clear();
            self.hints.insert(instance_scope, reason);
            return HintEnqueue::EscalatedToInstance;
        }

        self.hints.insert(hint.scope, hint.reason);
        HintEnqueue::Added
    }

    pub fn drain(&mut self, max_hints: usize) -> Vec<DirtyHint> {
        if max_hints == 0 {
            return Vec::new();
        }
        let scopes: Vec<_> = self.hints.keys().take(max_hints).cloned().collect();
        scopes
            .into_iter()
            .map(|scope| DirtyHint {
                reason: self
                    .hints
                    .remove(&scope)
                    .expect("dirty scope selected from the same map"),
                scope,
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.hints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }

    pub fn requires_overflow_reconcile(&self) -> bool {
        self.hints.values().any(|reason| reason.is_overflow_class())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPhase {
    Discovered,
    WatchRegistered,
    Scanning,
    Replaying,
    Reconciling,
    Live { commit_seq: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAction {
    Buffered(HintEnqueue),
    DeliverNow(DirtyHint),
    Reconcile(Vec<DirtyHint>),
    AwaitMoreReconcile,
    Live { commit_seq: u64 },
}

/// Explicit startup state machine that makes scanning before watcher
/// registration impossible.
pub struct WatchBeforeScan {
    phase: StartupPhase,
    buffered: DirtyCoalescer,
}

impl WatchBeforeScan {
    pub fn new(instance_key: Vec<u8>, hint_capacity: usize) -> Result<Self, SourceDriverError> {
        Ok(Self {
            phase: StartupPhase::Discovered,
            buffered: DirtyCoalescer::new(instance_key, hint_capacity)?,
        })
    }

    pub fn phase(&self) -> StartupPhase {
        self.phase
    }

    pub fn watcher_registered(&mut self) -> Result<(), SourceDriverError> {
        self.require_phase(StartupPhase::Discovered, "register watcher")?;
        self.phase = StartupPhase::WatchRegistered;
        Ok(())
    }

    pub fn begin_scan(&mut self) -> Result<(), SourceDriverError> {
        self.require_phase(StartupPhase::WatchRegistered, "begin scan")?;
        self.phase = StartupPhase::Scanning;
        Ok(())
    }

    pub fn finish_scan(&mut self) -> Result<(), SourceDriverError> {
        self.require_phase(StartupPhase::Scanning, "finish scan")?;
        self.phase = StartupPhase::Replaying;
        Ok(())
    }

    pub fn push_hint(&mut self, hint: DirtyHint) -> Result<StartupAction, SourceDriverError> {
        match self.phase {
            StartupPhase::WatchRegistered
            | StartupPhase::Scanning
            | StartupPhase::Replaying
            | StartupPhase::Reconciling => Ok(StartupAction::Buffered(self.buffered.enqueue(hint))),
            StartupPhase::Live { .. } => Ok(StartupAction::DeliverNow(hint)),
            StartupPhase::Discovered => Err(SourceDriverError::InvalidConfig(
                "cannot accept watcher hints before watcher registration".to_string(),
            )),
        }
    }

    pub fn next_reconcile_batch(
        &mut self,
        max_hints: usize,
    ) -> Result<StartupAction, SourceDriverError> {
        if !matches!(
            self.phase,
            StartupPhase::Replaying | StartupPhase::Reconciling
        ) {
            return Err(SourceDriverError::InvalidConfig(
                "reconcile can only follow the initial scan".to_string(),
            ));
        }
        self.phase = StartupPhase::Reconciling;
        let hints = self.buffered.drain(max_hints);
        if hints.is_empty() {
            Ok(StartupAction::AwaitMoreReconcile)
        } else {
            Ok(StartupAction::Reconcile(hints))
        }
    }

    /// Marks the current reconcile batch complete. Hints buffered during the
    /// batch force another pass; otherwise the known commit becomes Live.
    pub fn finish_reconcile(
        &mut self,
        commit_seq: u64,
    ) -> Result<StartupAction, SourceDriverError> {
        self.require_phase(StartupPhase::Reconciling, "finish reconcile")?;
        if self.buffered.is_empty() {
            self.phase = StartupPhase::Live { commit_seq };
            Ok(StartupAction::Live { commit_seq })
        } else {
            self.phase = StartupPhase::Replaying;
            Ok(StartupAction::AwaitMoreReconcile)
        }
    }

    fn require_phase(
        &self,
        expected: StartupPhase,
        operation: &'static str,
    ) -> Result<(), SourceDriverError> {
        if self.phase == expected {
            Ok(())
        } else {
            Err(SourceDriverError::InvalidConfig(format!(
                "cannot {operation} while startup phase is {:?}",
                self.phase
            )))
        }
    }
}

/// Pure timer policy used by a host runtime. It adds an active-object cadence,
/// a shorter incomplete-tail retry, and watcher-failure fallback without
/// storing wall-clock assessments as durable source facts.
#[derive(Debug, Clone)]
pub struct PollingPolicy {
    pub active_interval_ms: u64,
    pub idle_interval_ms: u64,
    pub incomplete_retry_ms: u64,
    pub active_window_ms: u64,
    pub failures_before_fallback: u32,
    active_until_ms: u64,
    watcher_failures: u32,
    has_incomplete_tail: bool,
}

impl Default for PollingPolicy {
    fn default() -> Self {
        Self {
            active_interval_ms: 500,
            idle_interval_ms: 5_000,
            incomplete_retry_ms: 50,
            active_window_ms: 5 * 60_000,
            failures_before_fallback: 3,
            active_until_ms: 0,
            watcher_failures: 0,
            has_incomplete_tail: false,
        }
    }
}

impl PollingPolicy {
    pub fn record_activity(&mut self, now_ms: u64) {
        self.active_until_ms = now_ms.saturating_add(self.active_window_ms);
    }

    pub fn record_watcher_failure(&mut self) {
        self.watcher_failures = self.watcher_failures.saturating_add(1);
    }

    /// Enter polling fallback immediately when the watcher backend could not be
    /// created or registered at all. Runtime event failures still use the
    /// repeated-failure threshold above.
    pub fn record_watcher_unavailable(&mut self) {
        self.watcher_failures = self.failures_before_fallback;
    }

    pub fn record_watcher_success(&mut self) {
        self.watcher_failures = 0;
    }

    pub fn set_incomplete_tail(&mut self, present: bool) {
        self.has_incomplete_tail = present;
    }

    pub fn fallback_active(&self) -> bool {
        self.watcher_failures >= self.failures_before_fallback
    }

    pub fn next_delay_ms(&self, now_ms: u64) -> u64 {
        if self.has_incomplete_tail {
            self.incomplete_retry_ms
        } else if self.fallback_active() || now_ms <= self.active_until_ms {
            self.active_interval_ms
        } else {
            self.idle_interval_ms
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(key: u8, reason: DirtyReason) -> DirtyHint {
        DirtyHint {
            scope: DirtyScope::Object(vec![key]),
            reason,
        }
    }

    #[test]
    fn duplicate_hints_coalesce_and_keep_strongest_reason() {
        let mut hints = DirtyCoalescer::new(b"instance".to_vec(), 4).unwrap();
        assert_eq!(
            hints.enqueue(hint(1, DirtyReason::NativeEvent)),
            HintEnqueue::Added
        );
        assert_eq!(
            hints.enqueue(hint(1, DirtyReason::WatcherOverflow)),
            HintEnqueue::Coalesced
        );
        let drained = hints.drain(4);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].reason, DirtyReason::WatcherOverflow);
    }

    #[test]
    fn capacity_overflow_collapses_to_instance_reconcile() {
        let mut hints = DirtyCoalescer::new(b"instance".to_vec(), 2).unwrap();
        hints.enqueue(hint(1, DirtyReason::NativeEvent));
        hints.enqueue(hint(2, DirtyReason::NativeEvent));
        assert_eq!(
            hints.enqueue(hint(3, DirtyReason::NativeEvent)),
            HintEnqueue::EscalatedToInstance
        );
        let drained = hints.drain(2);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].scope, DirtyScope::Instance(b"instance".to_vec()));
        assert_eq!(drained[0].reason, DirtyReason::InternalQueueOverflow);
    }

    #[test]
    fn startup_cannot_scan_before_registering_a_watcher() {
        let mut startup = WatchBeforeScan::new(b"instance".to_vec(), 4).unwrap();
        assert!(startup.begin_scan().is_err());
        startup.watcher_registered().unwrap();
        startup.begin_scan().unwrap();
        assert_eq!(startup.phase(), StartupPhase::Scanning);
    }

    #[test]
    fn startup_replays_hints_arriving_during_scan_and_reconcile() {
        let mut startup = WatchBeforeScan::new(b"instance".to_vec(), 4).unwrap();
        startup.watcher_registered().unwrap();
        startup.begin_scan().unwrap();
        startup
            .push_hint(hint(1, DirtyReason::NativeEvent))
            .unwrap();
        startup.finish_scan().unwrap();
        assert!(matches!(
            startup.next_reconcile_batch(4).unwrap(),
            StartupAction::Reconcile(ref hints) if hints.len() == 1
        ));
        startup
            .push_hint(hint(2, DirtyReason::NativeEvent))
            .unwrap();
        assert_eq!(
            startup.finish_reconcile(10).unwrap(),
            StartupAction::AwaitMoreReconcile
        );
        assert!(matches!(
            startup.next_reconcile_batch(4).unwrap(),
            StartupAction::Reconcile(ref hints) if hints.len() == 1
        ));
        assert_eq!(
            startup.finish_reconcile(11).unwrap(),
            StartupAction::Live { commit_seq: 11 }
        );
    }

    #[test]
    fn incomplete_tail_gets_short_retry_without_an_event() {
        let mut policy = PollingPolicy::default();
        policy.set_incomplete_tail(true);
        assert_eq!(policy.next_delay_ms(1_000), 50);
        policy.set_incomplete_tail(false);
        assert_eq!(policy.next_delay_ms(1_000), 5_000);
        policy.record_activity(1_000);
        assert_eq!(policy.next_delay_ms(1_001), 500);
    }

    #[test]
    fn repeated_watcher_failure_enables_polling_fallback_until_success() {
        let mut policy = PollingPolicy::default();
        policy.record_watcher_failure();
        policy.record_watcher_failure();
        assert!(!policy.fallback_active());
        policy.record_watcher_failure();
        assert!(policy.fallback_active());
        assert_eq!(policy.next_delay_ms(10_000), 500);
        policy.record_watcher_success();
        assert!(!policy.fallback_active());
        assert_eq!(policy.next_delay_ms(10_000), 5_000);
    }
}
