//! Engine-owned observation lifecycle and dirty-instance admission.
//!
//! Watchers, polling, and explicit repair all converge on the coordinator.
//! This runtime serializes those coordinator passes and retains bounded dirty
//! state whenever work cannot be admitted or a pass cannot prove itself live.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::source::DirtyReason;

use super::{EngineError, QueryCancellationToken, ReconcileOutcome};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

const DEFAULT_DIRTY_INSTANCE_CAPACITY: usize = 1_024;
const MAX_ERROR_DETAIL_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservationPhase {
    Idle,
    Scanning,
    Reconciling,
    Live,
    Dirty,
    Degraded,
    Stopped,
}

impl ObservationPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Scanning => "scanning",
            Self::Reconciling => "reconciling",
            Self::Live => "live",
            Self::Dirty => "dirty",
            Self::Degraded => "degraded",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ObservationStatusSnapshot {
    pub state: String,
    pub reconcile_in_flight: bool,
    pub dirty_instances: u32,
    pub full_reconcile_required: bool,
    pub recovery_required: bool,
    pub supervisors_running: u32,
    pub watched_instances: u32,
    pub watch_roots: u32,
    pub reconciles_total: u64,
    pub failed_reconciles_total: u64,
    pub retry_signals_total: u64,
    pub queue_overflows_total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_commit_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_started_at_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_finished_at_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct InstanceKey {
    adapter_id: String,
    stable_key: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ObjectKey {
    instance: InstanceKey,
    stream_key: String,
    object_key: Vec<u8>,
}

impl InstanceKey {
    fn new(adapter_id: &str, stable_key: &[u8]) -> Result<Self, EngineError> {
        if adapter_id.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation adapter ID must not be empty".to_string(),
            ));
        }
        if stable_key.is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation source-instance key must not be empty".to_string(),
            ));
        }
        Ok(Self {
            adapter_id: adapter_id.to_string(),
            stable_key: stable_key.to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReconcileScope {
    Adapter(String),
    Instance(InstanceKey),
    Object(ObjectKey),
}

#[derive(Debug, Clone, Copy)]
struct PendingDirty {
    reason: DirtyReason,
    sequence: u64,
}

#[derive(Debug, Clone)]
struct ActiveReconcile {
    id: u64,
    scope: ReconcileScope,
    dirty_sequence_at_start: u64,
}

struct ObservationState {
    accepting: bool,
    phase: ObservationPhase,
    active: Option<ActiveReconcile>,
    pending_instances: BTreeMap<InstanceKey, PendingDirty>,
    pending_objects: BTreeMap<ObjectKey, PendingDirty>,
    pending_adapters: BTreeMap<String, PendingDirty>,
    next_dirty_sequence: u64,
    next_reconcile_id: u64,
    reconciles_total: u64,
    failed_reconciles_total: u64,
    retry_signals_total: u64,
    queue_overflows_total: u64,
    last_commit_seq: Option<u64>,
    last_started_at_unix_ms: Option<i64>,
    last_finished_at_unix_ms: Option<i64>,
    last_error: Option<String>,
}

pub(crate) struct ObservationRuntime {
    dirty_instance_capacity: usize,
    state: Mutex<ObservationState>,
    idle: Condvar,
}

#[derive(Debug, Clone)]
pub(crate) enum PendingObservationWork {
    Adapter {
        adapter_id: String,
        reason: DirtyReason,
    },
    Instance {
        adapter_id: String,
        stable_key: Vec<u8>,
        reason: DirtyReason,
    },
    Object {
        adapter_id: String,
        stable_key: Vec<u8>,
        stream_key: String,
        object_key: Vec<u8>,
        reason: DirtyReason,
    },
}

impl ObservationRuntime {
    pub(crate) fn new() -> Arc<Self> {
        Self::with_capacity(DEFAULT_DIRTY_INSTANCE_CAPACITY)
            .expect("default observation dirty capacity must be valid")
    }

    fn with_capacity(dirty_instance_capacity: usize) -> Result<Arc<Self>, EngineError> {
        if dirty_instance_capacity == 0 {
            return Err(EngineError::InvalidConfig(
                "observation dirty-instance capacity must be greater than zero".to_string(),
            ));
        }
        Ok(Arc::new(Self {
            dirty_instance_capacity,
            state: Mutex::new(ObservationState {
                accepting: true,
                phase: ObservationPhase::Idle,
                active: None,
                pending_instances: BTreeMap::new(),
                pending_objects: BTreeMap::new(),
                pending_adapters: BTreeMap::new(),
                next_dirty_sequence: 0,
                next_reconcile_id: 0,
                reconciles_total: 0,
                failed_reconciles_total: 0,
                retry_signals_total: 0,
                queue_overflows_total: 0,
                last_commit_seq: None,
                last_started_at_unix_ms: None,
                last_finished_at_unix_ms: None,
                last_error: None,
            }),
            idle: Condvar::new(),
        }))
    }

    pub(crate) fn begin_full(
        self: &Arc<Self>,
        adapter_id: &str,
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        if adapter_id.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation adapter ID must not be empty".to_string(),
            ));
        }
        self.begin(
            ReconcileScope::Adapter(adapter_id.to_string()),
            ObservationPhase::Scanning,
            started_at_unix_ms,
        )
    }

    pub(crate) fn begin_instance(
        self: &Arc<Self>,
        adapter_id: &str,
        stable_key: &[u8],
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        self.begin(
            ReconcileScope::Instance(InstanceKey::new(adapter_id, stable_key)?),
            ObservationPhase::Reconciling,
            started_at_unix_ms,
        )
    }

    pub(crate) fn begin_object(
        self: &Arc<Self>,
        adapter_id: &str,
        stable_key: &[u8],
        stream_key: &str,
        object_key: &[u8],
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        if stream_key.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation source-stream key must not be empty".to_string(),
            ));
        }
        if object_key.is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation source-object key must not be empty".to_string(),
            ));
        }
        self.begin(
            ReconcileScope::Object(ObjectKey {
                instance: InstanceKey::new(adapter_id, stable_key)?,
                stream_key: stream_key.to_string(),
                object_key: object_key.to_vec(),
            }),
            ObservationPhase::Reconciling,
            started_at_unix_ms,
        )
    }

    fn begin(
        self: &Arc<Self>,
        scope: ReconcileScope,
        phase: ObservationPhase,
        started_at_unix_ms: i64,
    ) -> Result<ObservationLease, EngineError> {
        let mut state = self.lock_state();
        if !state.accepting {
            return Err(EngineError::ShuttingDown);
        }
        if state.active.is_some() {
            self.enqueue_scope_locked(&mut state, &scope, DirtyReason::ManualRepair);
            return Err(EngineError::ObservationBusy);
        }

        state.next_reconcile_id = state.next_reconcile_id.saturating_add(1);
        let id = state.next_reconcile_id;
        state.reconciles_total = state.reconciles_total.saturating_add(1);
        state.last_started_at_unix_ms = Some(started_at_unix_ms);
        state.phase = phase;
        state.active = Some(ActiveReconcile {
            id,
            scope,
            dirty_sequence_at_start: state.next_dirty_sequence,
        });
        drop(state);

        Ok(ObservationLease {
            runtime: Arc::clone(self),
            id,
            finished: false,
        })
    }

    pub(crate) fn snapshot(&self) -> ObservationStatusSnapshot {
        let state = self.lock_state();
        ObservationStatusSnapshot {
            state: state.phase.as_str().to_string(),
            reconcile_in_flight: state.active.is_some(),
            dirty_instances: dirty_instance_count(&state),
            full_reconcile_required: !state.pending_adapters.is_empty(),
            recovery_required: has_degraded_pending(&state),
            supervisors_running: 0,
            watched_instances: 0,
            watch_roots: 0,
            reconciles_total: state.reconciles_total,
            failed_reconciles_total: state.failed_reconciles_total,
            retry_signals_total: state.retry_signals_total,
            queue_overflows_total: state.queue_overflows_total,
            last_commit_seq: state.last_commit_seq,
            last_started_at_unix_ms: state.last_started_at_unix_ms,
            last_finished_at_unix_ms: state.last_finished_at_unix_ms,
            last_error: state.last_error.clone(),
        }
    }

    pub(crate) fn mark_instance_dirty(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
        reason: DirtyReason,
    ) -> Result<(), EngineError> {
        let key = InstanceKey::new(adapter_id, stable_key)?;
        let mut state = self.lock_state();
        if !state.accepting {
            return Err(EngineError::ShuttingDown);
        }
        self.enqueue_instance_locked(&mut state, key, reason);
        if state.active.is_none() {
            state.phase = pending_phase(&state);
        }
        Ok(())
    }

    pub(crate) fn mark_object_dirty(
        &self,
        adapter_id: &str,
        stable_key: &[u8],
        stream_key: &str,
        object_key: &[u8],
        reason: DirtyReason,
    ) -> Result<(), EngineError> {
        if stream_key.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation source-stream key must not be empty".to_string(),
            ));
        }
        if object_key.is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation source-object key must not be empty".to_string(),
            ));
        }
        let key = ObjectKey {
            instance: InstanceKey::new(adapter_id, stable_key)?,
            stream_key: stream_key.to_string(),
            object_key: object_key.to_vec(),
        };
        let mut state = self.lock_state();
        if !state.accepting {
            return Err(EngineError::ShuttingDown);
        }
        self.enqueue_object_locked(&mut state, key, reason);
        if state.active.is_none() {
            state.phase = pending_phase(&state);
        }
        Ok(())
    }

    pub(crate) fn mark_adapter_dirty(
        &self,
        adapter_id: &str,
        reason: DirtyReason,
    ) -> Result<(), EngineError> {
        if adapter_id.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "observation adapter ID must not be empty".to_string(),
            ));
        }
        let mut state = self.lock_state();
        if !state.accepting {
            return Err(EngineError::ShuttingDown);
        }
        enqueue_adapter_locked(&mut state, adapter_id, reason);
        if state.active.is_none() {
            state.phase = pending_phase(&state);
        }
        Ok(())
    }

    pub(crate) fn stop_and_wait(&self) {
        let mut state = self.lock_state();
        state.accepting = false;
        while state.active.is_some() {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        state.phase = ObservationPhase::Stopped;
    }

    pub(crate) fn next_pending(&self, adapter_id: &str) -> Option<PendingObservationWork> {
        let state = self.lock_state();
        if !state.accepting || state.active.is_some() {
            return None;
        }
        if let Some(pending) = state.pending_adapters.get(adapter_id) {
            return Some(PendingObservationWork::Adapter {
                adapter_id: adapter_id.to_string(),
                reason: pending.reason,
            });
        }
        let instance = state
            .pending_instances
            .iter()
            .filter(|(key, _)| key.adapter_id == adapter_id)
            .min_by_key(|(_, pending)| pending.sequence);
        let object = state
            .pending_objects
            .iter()
            .filter(|(key, _)| key.instance.adapter_id == adapter_id)
            .min_by_key(|(_, pending)| pending.sequence);
        match (instance, object) {
            (Some((key, pending)), Some((_, object_pending)))
                if pending.sequence <= object_pending.sequence =>
            {
                Some(PendingObservationWork::Instance {
                    adapter_id: key.adapter_id.clone(),
                    stable_key: key.stable_key.clone(),
                    reason: pending.reason,
                })
            }
            (Some((key, pending)), None) => Some(PendingObservationWork::Instance {
                adapter_id: key.adapter_id.clone(),
                stable_key: key.stable_key.clone(),
                reason: pending.reason,
            }),
            (_, Some((key, pending))) => Some(PendingObservationWork::Object {
                adapter_id: key.instance.adapter_id.clone(),
                stable_key: key.instance.stable_key.clone(),
                stream_key: key.stream_key.clone(),
                object_key: key.object_key.clone(),
                reason: pending.reason,
            }),
            (None, None) => None,
        }
    }

    pub(crate) fn has_pending(&self, adapter_id: &str) -> bool {
        let state = self.lock_state();
        state.pending_adapters.contains_key(adapter_id)
            || state
                .pending_instances
                .keys()
                .any(|key| key.adapter_id == adapter_id)
            || state
                .pending_objects
                .keys()
                .any(|key| key.instance.adapter_id == adapter_id)
    }

    pub(crate) fn wait_until_idle_cancellable(
        &self,
        cancellations: &[QueryCancellationToken],
    ) -> Result<(), EngineError> {
        let mut state = self.lock_state();
        loop {
            if cancellations
                .iter()
                .any(QueryCancellationToken::is_cancelled)
            {
                return Err(EngineError::QueryCancelled);
            }
            if !state.accepting {
                return Err(EngineError::ShuttingDown);
            }
            if state.active.is_none() {
                return Ok(());
            }
            let (next, _) = self
                .idle
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
    }

    fn transition_to_reconciling(&self, id: u64) {
        let mut state = self.lock_state();
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            state.phase = ObservationPhase::Reconciling;
        }
    }

    fn finish_success(
        &self,
        id: u64,
        outcome: &ReconcileOutcome,
        commit_seq: u64,
        finished_at_unix_ms: i64,
    ) {
        let mut state = self.lock_state();
        let Some(active) = state.active.take() else {
            return;
        };
        if active.id != id {
            state.active = Some(active);
            return;
        }

        acknowledge_scope_locked(&mut state, &active.scope, active.dirty_sequence_at_start);
        state.last_commit_seq = Some(commit_seq);
        state.last_finished_at_unix_ms = Some(finished_at_unix_ms);
        state.retry_signals_total = state
            .retry_signals_total
            .saturating_add(u64::from(outcome.retries_required));
        let retry_adapter_id = match &active.scope {
            ReconcileScope::Adapter(adapter_id) => adapter_id.clone(),
            ReconcileScope::Instance(key) => key.adapter_id.clone(),
            ReconcileScope::Object(key) => key.instance.adapter_id.clone(),
        };
        for target in &outcome.retry_targets {
            let key = ObjectKey {
                instance: InstanceKey {
                    adapter_id: retry_adapter_id.clone(),
                    stable_key: target.stable_key.clone(),
                },
                stream_key: target.stream_key.clone(),
                object_key: target.object_key.clone(),
            };
            self.enqueue_object_locked(&mut state, key, DirtyReason::Recovery);
        }
        let targeted = u32::try_from(outcome.retry_targets.len()).unwrap_or(u32::MAX);
        if outcome.retries_required > targeted {
            self.enqueue_scope_locked(&mut state, &active.scope, DirtyReason::Recovery);
        }
        state.phase = pending_phase(&state);
        state.last_error = None;
        self.idle.notify_all();
    }

    fn finish_failure(&self, id: u64, detail: &str, finished_at_unix_ms: i64) {
        let mut state = self.lock_state();
        let Some(active) = state.active.take() else {
            return;
        };
        if active.id != id {
            state.active = Some(active);
            return;
        }

        state.failed_reconciles_total = state.failed_reconciles_total.saturating_add(1);
        state.last_finished_at_unix_ms = Some(finished_at_unix_ms);
        state.last_error = Some(bounded_detail(detail));
        self.enqueue_scope_locked(&mut state, &active.scope, DirtyReason::Recovery);
        state.phase = ObservationPhase::Degraded;
        self.idle.notify_all();
    }

    fn enqueue_scope_locked(
        &self,
        state: &mut ObservationState,
        scope: &ReconcileScope,
        reason: DirtyReason,
    ) {
        match scope {
            ReconcileScope::Adapter(adapter_id) => {
                enqueue_adapter_locked(state, adapter_id, reason);
            }
            ReconcileScope::Instance(key) => {
                self.enqueue_instance_locked(state, key.clone(), reason);
            }
            ReconcileScope::Object(key) => {
                self.enqueue_object_locked(state, key.clone(), reason);
            }
        }
        if state.active.is_none() {
            state.phase = pending_phase(state);
        }
    }

    fn enqueue_instance_locked(
        &self,
        state: &mut ObservationState,
        key: InstanceKey,
        reason: DirtyReason,
    ) {
        let sequence = next_dirty_sequence(state);
        if let Some(adapter) = state.pending_adapters.get_mut(&key.adapter_id) {
            adapter.reason = adapter.reason.merge(reason);
            adapter.sequence = sequence;
            return;
        }
        if let Some(pending) = state.pending_instances.get_mut(&key) {
            pending.reason = pending.reason.merge(reason);
            pending.sequence = sequence;
            return;
        }
        let mut merged = reason;
        state.pending_objects.retain(|object_key, pending| {
            if object_key.instance == key {
                merged = merged.merge(pending.reason);
                false
            } else {
                true
            }
        });
        if state.pending_instances.len() + state.pending_objects.len()
            < self.dirty_instance_capacity
        {
            state.pending_instances.insert(
                key,
                PendingDirty {
                    reason: merged,
                    sequence,
                },
            );
            return;
        }

        state.queue_overflows_total = state.queue_overflows_total.saturating_add(1);
        let adapter_id = key.adapter_id;
        let mut merged = DirtyReason::InternalQueueOverflow.merge(reason);
        retain_other_adapter_work(state, &adapter_id, &mut merged);
        enqueue_adapter_with_sequence_locked(state, adapter_id, merged, sequence);
    }

    fn enqueue_object_locked(
        &self,
        state: &mut ObservationState,
        key: ObjectKey,
        reason: DirtyReason,
    ) {
        let sequence = next_dirty_sequence(state);
        if let Some(adapter) = state.pending_adapters.get_mut(&key.instance.adapter_id) {
            adapter.reason = adapter.reason.merge(reason);
            adapter.sequence = sequence;
            return;
        }
        if let Some(instance) = state.pending_instances.get_mut(&key.instance) {
            instance.reason = instance.reason.merge(reason);
            instance.sequence = sequence;
            return;
        }
        if let Some(pending) = state.pending_objects.get_mut(&key) {
            pending.reason = pending.reason.merge(reason);
            pending.sequence = sequence;
            return;
        }
        if state.pending_instances.len() + state.pending_objects.len()
            < self.dirty_instance_capacity
        {
            state
                .pending_objects
                .insert(key, PendingDirty { reason, sequence });
            return;
        }

        state.queue_overflows_total = state.queue_overflows_total.saturating_add(1);
        let adapter_id = key.instance.adapter_id;
        let mut merged = DirtyReason::InternalQueueOverflow.merge(reason);
        retain_other_adapter_work(state, &adapter_id, &mut merged);
        enqueue_adapter_with_sequence_locked(state, adapter_id, merged, sequence);
    }

    fn lock_state(&self) -> MutexGuard<'_, ObservationState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub(crate) struct ObservationLease {
    runtime: Arc<ObservationRuntime>,
    id: u64,
    finished: bool,
}

impl ObservationLease {
    pub(crate) fn begin_reconciling(&self) {
        self.runtime.transition_to_reconciling(self.id);
    }

    pub(crate) fn complete(
        mut self,
        outcome: &ReconcileOutcome,
        commit_seq: u64,
        finished_at_unix_ms: i64,
    ) {
        self.runtime
            .finish_success(self.id, outcome, commit_seq, finished_at_unix_ms);
        self.finished = true;
    }

    pub(crate) fn fail(mut self, error: &EngineError, finished_at_unix_ms: i64) {
        self.runtime
            .finish_failure(self.id, &error.to_string(), finished_at_unix_ms);
        self.finished = true;
    }
}

impl Drop for ObservationLease {
    fn drop(&mut self) {
        if !self.finished {
            self.runtime.finish_failure(
                self.id,
                "observation reconcile ended without a completion result",
                fallback_now_unix_ms(),
            );
        }
    }
}

fn enqueue_adapter_locked(state: &mut ObservationState, adapter_id: &str, reason: DirtyReason) {
    let sequence = next_dirty_sequence(state);
    let mut merged = reason;
    retain_other_adapter_work(state, adapter_id, &mut merged);
    enqueue_adapter_with_sequence_locked(state, adapter_id.to_string(), merged, sequence);
}

fn retain_other_adapter_work(
    state: &mut ObservationState,
    adapter_id: &str,
    merged: &mut DirtyReason,
) {
    state.pending_instances.retain(|key, pending| {
        if key.adapter_id == adapter_id {
            *merged = (*merged).merge(pending.reason);
            false
        } else {
            true
        }
    });
    state.pending_objects.retain(|key, pending| {
        if key.instance.adapter_id == adapter_id {
            *merged = (*merged).merge(pending.reason);
            false
        } else {
            true
        }
    });
}

fn enqueue_adapter_with_sequence_locked(
    state: &mut ObservationState,
    adapter_id: String,
    reason: DirtyReason,
    sequence: u64,
) {
    state
        .pending_adapters
        .entry(adapter_id)
        .and_modify(|pending| {
            pending.reason = pending.reason.merge(reason);
            pending.sequence = sequence;
        })
        .or_insert(PendingDirty { reason, sequence });
}

fn acknowledge_scope_locked(
    state: &mut ObservationState,
    scope: &ReconcileScope,
    through_sequence: u64,
) {
    match scope {
        ReconcileScope::Adapter(adapter_id) => {
            state.pending_instances.retain(|key, pending| {
                key.adapter_id != *adapter_id || pending.sequence > through_sequence
            });
            state.pending_objects.retain(|key, pending| {
                key.instance.adapter_id != *adapter_id || pending.sequence > through_sequence
            });
            if state
                .pending_adapters
                .get(adapter_id)
                .is_some_and(|pending| pending.sequence <= through_sequence)
            {
                state.pending_adapters.remove(adapter_id);
            }
        }
        ReconcileScope::Instance(key) => {
            if state
                .pending_instances
                .get(key)
                .is_some_and(|pending| pending.sequence <= through_sequence)
            {
                state.pending_instances.remove(key);
            }
            state.pending_objects.retain(|object_key, pending| {
                object_key.instance != *key || pending.sequence > through_sequence
            });
        }
        ReconcileScope::Object(key) => {
            if state
                .pending_objects
                .get(key)
                .is_some_and(|pending| pending.sequence <= through_sequence)
            {
                state.pending_objects.remove(key);
            }
        }
    }
}

fn next_dirty_sequence(state: &mut ObservationState) -> u64 {
    state.next_dirty_sequence = state.next_dirty_sequence.saturating_add(1);
    state.next_dirty_sequence
}

fn pending_phase(state: &ObservationState) -> ObservationPhase {
    let reasons = state
        .pending_instances
        .values()
        .chain(state.pending_objects.values())
        .chain(state.pending_adapters.values())
        .map(|pending| pending.reason);
    let mut has_pending = false;
    for reason in reasons {
        has_pending = true;
        if is_degraded_reason(reason) {
            return ObservationPhase::Degraded;
        }
    }
    if has_pending {
        ObservationPhase::Dirty
    } else {
        ObservationPhase::Live
    }
}

fn has_degraded_pending(state: &ObservationState) -> bool {
    state
        .pending_instances
        .values()
        .chain(state.pending_objects.values())
        .chain(state.pending_adapters.values())
        .any(|pending| is_degraded_reason(pending.reason))
}

fn is_degraded_reason(reason: DirtyReason) -> bool {
    matches!(
        reason,
        DirtyReason::WatcherOverflow
            | DirtyReason::InternalQueueOverflow
            | DirtyReason::BackendError
            | DirtyReason::CursorInvalid
            | DirtyReason::IdentityChanged
            | DirtyReason::RootMoved
            | DirtyReason::Recovery
    )
}

fn bounded_detail(detail: &str) -> String {
    if detail.chars().count() <= MAX_ERROR_DETAIL_CHARS {
        return detail.to_string();
    }
    let mut bounded: String = detail.chars().take(MAX_ERROR_DETAIL_CHARS - 3).collect();
    bounded.push_str("...");
    bounded
}

fn bounded_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn dirty_instance_count(state: &ObservationState) -> u32 {
    let mut instances = BTreeSet::new();
    instances.extend(state.pending_instances.keys().cloned());
    instances.extend(
        state
            .pending_objects
            .keys()
            .map(|object| object.instance.clone()),
    );
    bounded_u32(instances.len())
}

fn fallback_now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_outcome() -> ReconcileOutcome {
        ReconcileOutcome::default()
    }

    #[test]
    fn successful_full_reconcile_reaches_a_known_live_commit() {
        let runtime = ObservationRuntime::with_capacity(2).unwrap();
        let lease = runtime.begin_full("adapter", 10).unwrap();
        assert_eq!(runtime.snapshot().state, "scanning");
        lease.begin_reconciling();
        assert_eq!(runtime.snapshot().state, "reconciling");
        lease.complete(&empty_outcome(), 42, 20);

        let status = runtime.snapshot();
        assert_eq!(status.state, "live");
        assert_eq!(status.last_commit_seq, Some(42));
        assert_eq!(status.reconciles_total, 1);
        assert!(!status.reconcile_in_flight);
        assert!(!status.recovery_required);
    }

    #[test]
    fn incomplete_tail_retry_is_retained_as_object_work_not_full_reconcile() {
        let runtime = ObservationRuntime::with_capacity(4).unwrap();
        let lease = runtime.begin_full("adapter", 10).unwrap();
        let outcome = ReconcileOutcome {
            retries_required: 1,
            incomplete_tail_retries: 1,
            retry_targets: vec![super::super::ReconcileRetryTarget {
                stable_key: b"instance".to_vec(),
                stream_key: "transcript".to_string(),
                object_key: b"object".to_vec(),
            }],
            ..ReconcileOutcome::default()
        };
        lease.complete(&outcome, 1, 20);

        assert!(!runtime.snapshot().full_reconcile_required);
        assert!(matches!(
            runtime.next_pending("adapter"),
            Some(PendingObservationWork::Object {
                stable_key,
                stream_key,
                object_key,
                ..
            }) if stable_key == b"instance"
                && stream_key == "transcript"
                && object_key == b"object"
        ));

        let retry = runtime
            .begin_object("adapter", b"instance", "transcript", b"object", 30)
            .unwrap();
        retry.complete(&ReconcileOutcome::default(), 1, 40);
        assert!(runtime.next_pending("adapter").is_none());
    }

    #[test]
    fn object_retry_completion_does_not_acknowledge_sibling_backlog() {
        let runtime = ObservationRuntime::with_capacity(4).unwrap();
        let lease = runtime.begin_full("adapter", 10).unwrap();
        let target = |object_key: &[u8]| super::super::ReconcileRetryTarget {
            stable_key: b"instance".to_vec(),
            stream_key: "transcript".to_string(),
            object_key: object_key.to_vec(),
        };
        lease.complete(
            &ReconcileOutcome {
                retries_required: 2,
                backlog_remaining: 2,
                retry_targets: vec![target(b"first"), target(b"second")],
                ..ReconcileOutcome::default()
            },
            1,
            20,
        );

        let first = runtime
            .begin_object("adapter", b"instance", "transcript", b"first", 30)
            .unwrap();
        first.complete(&ReconcileOutcome::default(), 2, 40);

        assert!(matches!(
            runtime.next_pending("adapter"),
            Some(PendingObservationWork::Object { object_key, .. })
                if object_key == b"second"
        ));
        runtime
            .begin_object("adapter", b"instance", "transcript", b"second", 50)
            .unwrap()
            .complete(&ReconcileOutcome::default(), 3, 60);
        assert!(runtime.next_pending("adapter").is_none());
    }

    #[test]
    fn concurrent_requests_coalesce_and_overflow_to_adapter_reconcile() {
        let runtime = ObservationRuntime::with_capacity(2).unwrap();
        let lease = runtime.begin_instance("adapter", b"one", 10).unwrap();

        assert!(matches!(
            runtime.begin_instance("adapter", b"two", 11),
            Err(EngineError::ObservationBusy)
        ));
        assert!(matches!(
            runtime.begin_instance("adapter", b"three", 12),
            Err(EngineError::ObservationBusy)
        ));
        assert!(matches!(
            runtime.begin_instance("adapter", b"four", 13),
            Err(EngineError::ObservationBusy)
        ));
        let busy = runtime.snapshot();
        assert_eq!(busy.dirty_instances, 0);
        assert!(busy.full_reconcile_required);
        assert!(busy.recovery_required);
        assert_eq!(busy.queue_overflows_total, 1);

        lease.complete(&empty_outcome(), 7, 20);
        let pending = runtime.snapshot();
        assert_eq!(pending.state, "degraded");
        assert!(pending.full_reconcile_required);

        runtime
            .begin_full("adapter", 30)
            .unwrap()
            .complete(&empty_outcome(), 8, 40);
        assert_eq!(runtime.snapshot().state, "live");
    }

    #[test]
    fn hints_admitted_during_reconcile_survive_its_completion() {
        let runtime = ObservationRuntime::with_capacity(2).unwrap();
        let lease = runtime.begin_full("adapter", 10).unwrap();
        assert!(matches!(
            runtime.begin_instance("adapter", b"one", 11),
            Err(EngineError::ObservationBusy)
        ));
        lease.complete(&empty_outcome(), 1, 12);

        let status = runtime.snapshot();
        assert_eq!(status.state, "dirty");
        assert_eq!(status.dirty_instances, 1);
    }

    #[test]
    fn retry_and_failure_both_retain_recovery_state() {
        let runtime = ObservationRuntime::with_capacity(2).unwrap();
        let mut outcome = empty_outcome();
        outcome.retries_required = 2;
        runtime
            .begin_full("adapter", 10)
            .unwrap()
            .complete(&outcome, 1, 20);
        let retry = runtime.snapshot();
        assert_eq!(retry.state, "degraded");
        assert_eq!(retry.retry_signals_total, 2);
        assert!(retry.full_reconcile_required);
        assert!(retry.recovery_required);

        runtime
            .begin_full("adapter", 30)
            .unwrap()
            .fail(&EngineError::QueryCancelled, 40);
        let failed = runtime.snapshot();
        assert_eq!(failed.failed_reconciles_total, 1);
        assert!(failed.last_error.unwrap().contains("cancelled"));
    }

    #[test]
    fn shutdown_rejects_new_observation_work() {
        let runtime = ObservationRuntime::with_capacity(1).unwrap();
        runtime.stop_and_wait();
        assert_eq!(runtime.snapshot().state, "stopped");
        assert!(matches!(
            runtime.begin_full("adapter", 1),
            Err(EngineError::ShuttingDown)
        ));
    }

    #[test]
    fn public_dirty_admission_coalesces_without_exposing_instance_keys() {
        let runtime = ObservationRuntime::with_capacity(2).unwrap();
        runtime
            .mark_instance_dirty("adapter", b"one", DirtyReason::NativeEvent)
            .unwrap();
        runtime
            .mark_instance_dirty("adapter", b"one", DirtyReason::PollDetectedChange)
            .unwrap();
        assert_eq!(runtime.snapshot().dirty_instances, 1);
        assert_eq!(runtime.snapshot().state, "dirty");

        runtime
            .mark_adapter_dirty("adapter", DirtyReason::WatcherOverflow)
            .unwrap();
        let escalated = runtime.snapshot();
        assert_eq!(escalated.dirty_instances, 0);
        assert!(escalated.full_reconcile_required);
        assert_eq!(escalated.state, "degraded");
    }

    #[test]
    fn clean_reconcile_clears_the_previous_error_and_recovery_bit() {
        let runtime = ObservationRuntime::with_capacity(2).unwrap();
        runtime
            .begin_full("adapter", 10)
            .unwrap()
            .fail(&EngineError::QueryCancelled, 20);
        assert!(runtime.snapshot().last_error.is_some());

        runtime
            .begin_full("adapter", 30)
            .unwrap()
            .complete(&empty_outcome(), 7, 40);
        let recovered = runtime.snapshot();
        assert_eq!(recovered.state, "live");
        assert!(!recovered.recovery_required);
        assert!(recovered.last_error.is_none());
    }

    #[test]
    fn pending_work_prefers_adapter_recovery_and_is_hidden_during_reconcile() {
        let runtime = ObservationRuntime::with_capacity(4).unwrap();
        runtime
            .mark_instance_dirty("other", b"one", DirtyReason::NativeEvent)
            .unwrap();
        runtime
            .mark_adapter_dirty("adapter", DirtyReason::WatcherOverflow)
            .unwrap();
        assert!(matches!(
            runtime.next_pending("adapter"),
            Some(PendingObservationWork::Adapter { ref adapter_id, reason })
                if adapter_id == "adapter" && reason == DirtyReason::WatcherOverflow
        ));

        let lease = runtime.begin_full("adapter", 10).unwrap();
        assert!(runtime.next_pending("adapter").is_none());
        lease.complete(&empty_outcome(), 1, 20);
        assert!(matches!(
            runtime.next_pending("other"),
            Some(PendingObservationWork::Instance { ref adapter_id, ref stable_key, .. })
                if adapter_id == "other" && stable_key == b"one"
        ));
    }
}
