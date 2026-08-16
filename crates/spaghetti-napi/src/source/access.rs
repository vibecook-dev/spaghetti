//! Agent-neutral scope/dependency access budgets and bounded telemetry.
//!
//! A source or observer runtime reserves the declared worst-case access before
//! touching a native object, then commits the actual bytes and rows. Failed or
//! abandoned reservations consume their full reservation conservatively. This
//! prevents retries, panics, and partial reads from bypassing a scope budget.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

pub const ACCESS_TRACE_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_ACCESS_TRACE_CAPACITY: usize = 256;

const MAX_RELATION_ID_BYTES: usize = 128;
const MAX_TRACE_CAPACITY: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeAccessBounds {
    pub max_fan_out: u64,
    pub max_depth: u32,
    pub max_objects: u64,
    pub max_bytes: u64,
    pub max_rows: u64,
}

impl ScopeAccessBounds {
    pub fn validate(&self) -> Result<(), AccessBudgetError> {
        if self.max_fan_out == 0
            || self.max_depth == 0
            || self.max_objects == 0
            || self.max_bytes == 0
        {
            return Err(AccessBudgetError::InvalidConfig(
                "access fan-out, depth, object, and byte bounds must be greater than zero"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccessObjectToken([u8; 32]);

impl AccessObjectToken {
    pub fn derive(
        relation_id: &str,
        identity_components: &[&[u8]],
    ) -> Result<Self, AccessBudgetError> {
        validate_relation_id(relation_id)?;
        if identity_components.is_empty() {
            return Err(AccessBudgetError::InvalidConfig(
                "access object identity requires at least one component".to_string(),
            ));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"spaghetti/rfc012a/access-object/v1\0");
        hash_component(&mut hasher, relation_id.as_bytes());
        for component in identity_components {
            hash_component(&mut hasher, component);
        }
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessOperation {
    ObjectRead,
    ParameterizedQuery,
    ObjectListing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessPhase {
    Initial,
    Revalidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessOutcome {
    Available,
    Unavailable,
    Oversized,
    Failed,
    Abandoned,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLimit {
    MaxFanOut,
    MaxDepth,
    MaxObjects,
    MaxBytes,
    MaxRows,
    Reservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessTraceEntry {
    pub access_trace_contract_version: u32,
    pub sequence: u64,
    pub relation_id: String,
    pub operation: AccessOperation,
    pub phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub object_token: AccessObjectToken,
    pub depth: u32,
    pub reserved_bytes: u64,
    pub reserved_rows: u64,
    pub bytes_read: u64,
    pub rows_read: u64,
    pub outcome: AccessOutcome,
    pub denied_limit: Option<AccessLimit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessBudgetSnapshot {
    pub access_trace_contract_version: u32,
    pub relation_id: String,
    pub bounds: ScopeAccessBounds,
    pub attempts: u64,
    pub reservations_granted: u64,
    pub completed: u64,
    pub denied: u64,
    pub abandoned: u64,
    pub objects_accessed: u64,
    pub bytes_read: u64,
    pub rows_read: u64,
    pub max_depth_observed: u32,
    pub bytes_reserved: u64,
    pub rows_reserved: u64,
    pub trace_entries_dropped: u64,
    pub trace: Vec<AccessTraceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccessBudgetError {
    #[error("invalid access-budget configuration: {0}")]
    InvalidConfig(String),
    #[error("access relation {relation_id} exceeded {limit:?}")]
    LimitExceeded {
        relation_id: String,
        limit: AccessLimit,
    },
    #[error("actual access exceeded its reservation")]
    ActualExceedsReservation,
}

#[derive(Debug, Clone, Copy)]
pub struct AccessReservationRequest {
    pub operation: AccessOperation,
    pub phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub object_token: AccessObjectToken,
    pub depth: u32,
    pub max_bytes: u64,
    pub max_rows: u64,
}

#[derive(Clone)]
pub struct AccessBudget {
    inner: Arc<AccessBudgetInner>,
}

struct AccessBudgetInner {
    relation_id: String,
    bounds: ScopeAccessBounds,
    trace_capacity: usize,
    state: Mutex<AccessBudgetState>,
}

#[derive(Default)]
struct AccessBudgetState {
    next_sequence: u64,
    attempts: u64,
    reservations_granted: u64,
    completed: u64,
    denied: u64,
    abandoned: u64,
    committed_objects: BTreeSet<AccessObjectToken>,
    reserved_objects: BTreeMap<AccessObjectToken, u64>,
    committed_edges: BTreeSet<(Option<AccessObjectToken>, AccessObjectToken)>,
    reserved_edges: BTreeMap<(Option<AccessObjectToken>, AccessObjectToken), u64>,
    bytes_read: u64,
    rows_read: u64,
    bytes_reserved: u64,
    rows_reserved: u64,
    max_depth_observed: u32,
    trace_entries_dropped: u64,
    trace: VecDeque<AccessTraceEntry>,
}

impl AccessBudget {
    pub fn new(
        relation_id: impl Into<String>,
        bounds: ScopeAccessBounds,
    ) -> Result<Self, AccessBudgetError> {
        Self::with_trace_capacity(relation_id, bounds, DEFAULT_ACCESS_TRACE_CAPACITY)
    }

    pub fn with_trace_capacity(
        relation_id: impl Into<String>,
        bounds: ScopeAccessBounds,
        trace_capacity: usize,
    ) -> Result<Self, AccessBudgetError> {
        let relation_id = relation_id.into();
        validate_relation_id(&relation_id)?;
        bounds.validate()?;
        if trace_capacity == 0 || trace_capacity > MAX_TRACE_CAPACITY {
            return Err(AccessBudgetError::InvalidConfig(format!(
                "access trace capacity must be between 1 and {MAX_TRACE_CAPACITY}"
            )));
        }
        Ok(Self {
            inner: Arc::new(AccessBudgetInner {
                relation_id,
                bounds,
                trace_capacity,
                state: Mutex::new(AccessBudgetState::default()),
            }),
        })
    }

    pub fn relation_id(&self) -> &str {
        &self.inner.relation_id
    }

    pub fn reserve(
        &self,
        request: AccessReservationRequest,
    ) -> Result<AccessReservation, AccessBudgetError> {
        let mut state = self.inner.lock_state();
        state.attempts = state.attempts.saturating_add(1);
        state.next_sequence = state.next_sequence.saturating_add(1);
        let sequence = state.next_sequence;

        let denied_limit = self.denied_limit(&state, &request);
        if let Some(limit) = denied_limit {
            state.denied = state.denied.saturating_add(1);
            self.inner.push_trace(
                &mut state,
                AccessTraceEntry {
                    access_trace_contract_version: ACCESS_TRACE_CONTRACT_VERSION,
                    sequence,
                    relation_id: self.inner.relation_id.clone(),
                    operation: request.operation,
                    phase: request.phase,
                    parent_token: request.parent_token,
                    object_token: request.object_token,
                    depth: request.depth,
                    reserved_bytes: request.max_bytes,
                    reserved_rows: request.max_rows,
                    bytes_read: 0,
                    rows_read: 0,
                    outcome: AccessOutcome::Denied,
                    denied_limit: Some(limit),
                },
            );
            return Err(AccessBudgetError::LimitExceeded {
                relation_id: self.inner.relation_id.clone(),
                limit,
            });
        }

        state.reservations_granted = state.reservations_granted.saturating_add(1);
        state.bytes_reserved = state.bytes_reserved.saturating_add(request.max_bytes);
        state.rows_reserved = state.rows_reserved.saturating_add(request.max_rows);
        let object_reservations = state
            .reserved_objects
            .entry(request.object_token)
            .or_default();
        *object_reservations = object_reservations.saturating_add(1);
        let edge_reservations = state
            .reserved_edges
            .entry((request.parent_token, request.object_token))
            .or_default();
        *edge_reservations = edge_reservations.saturating_add(1);
        drop(state);

        Ok(AccessReservation {
            inner: Arc::clone(&self.inner),
            request,
            sequence,
            finished: false,
        })
    }

    fn denied_limit(
        &self,
        state: &AccessBudgetState,
        request: &AccessReservationRequest,
    ) -> Option<AccessLimit> {
        if request.depth == 0 || request.depth > self.inner.bounds.max_depth {
            return Some(AccessLimit::MaxDepth);
        }
        let object_is_new = !state.committed_objects.contains(&request.object_token)
            && !state.reserved_objects.contains_key(&request.object_token);
        let object_count = state.committed_objects.len() as u64
            + state
                .reserved_objects
                .keys()
                .filter(|token| !state.committed_objects.contains(token))
                .count() as u64
            + u64::from(object_is_new);
        if object_count > self.inner.bounds.max_objects {
            return Some(AccessLimit::MaxObjects);
        }

        let edge = (request.parent_token, request.object_token);
        let edge_is_new =
            !state.committed_edges.contains(&edge) && !state.reserved_edges.contains_key(&edge);
        let fan_out = state
            .committed_edges
            .iter()
            .filter(|(parent, _)| *parent == request.parent_token)
            .count() as u64
            + state
                .reserved_edges
                .keys()
                .filter(|edge| {
                    edge.0 == request.parent_token && !state.committed_edges.contains(edge)
                })
                .count() as u64
            + u64::from(edge_is_new);
        if fan_out > self.inner.bounds.max_fan_out {
            return Some(AccessLimit::MaxFanOut);
        }
        if sum_exceeds(
            self.inner.bounds.max_bytes,
            [state.bytes_read, state.bytes_reserved, request.max_bytes],
        ) {
            return Some(AccessLimit::MaxBytes);
        }
        if sum_exceeds(
            self.inner.bounds.max_rows,
            [state.rows_read, state.rows_reserved, request.max_rows],
        ) {
            return Some(AccessLimit::MaxRows);
        }
        None
    }

    pub fn snapshot(&self) -> AccessBudgetSnapshot {
        let state = self.inner.lock_state();
        let mut trace = state.trace.iter().cloned().collect::<Vec<_>>();
        trace.sort_by_key(|entry| entry.sequence);
        AccessBudgetSnapshot {
            access_trace_contract_version: ACCESS_TRACE_CONTRACT_VERSION,
            relation_id: self.inner.relation_id.clone(),
            bounds: self.inner.bounds,
            attempts: state.attempts,
            reservations_granted: state.reservations_granted,
            completed: state.completed,
            denied: state.denied,
            abandoned: state.abandoned,
            objects_accessed: state.committed_objects.len() as u64,
            bytes_read: state.bytes_read,
            rows_read: state.rows_read,
            max_depth_observed: state.max_depth_observed,
            bytes_reserved: state.bytes_reserved,
            rows_reserved: state.rows_reserved,
            trace_entries_dropped: state.trace_entries_dropped,
            trace,
        }
    }
}

impl AccessBudgetInner {
    fn lock_state(&self) -> MutexGuard<'_, AccessBudgetState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push_trace(&self, state: &mut AccessBudgetState, entry: AccessTraceEntry) {
        if state.trace.len() == self.trace_capacity {
            state.trace.pop_front();
            state.trace_entries_dropped = state.trace_entries_dropped.saturating_add(1);
        }
        state.trace.push_back(entry);
    }
}

pub struct AccessReservation {
    inner: Arc<AccessBudgetInner>,
    request: AccessReservationRequest,
    sequence: u64,
    finished: bool,
}

impl AccessReservation {
    pub fn complete(
        mut self,
        bytes_read: u64,
        rows_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        if matches!(outcome, AccessOutcome::Denied | AccessOutcome::Abandoned) {
            return Err(AccessBudgetError::InvalidConfig(
                "a granted reservation cannot complete as denied or abandoned".to_string(),
            ));
        }
        self.finished = true;
        self.finish(bytes_read, rows_read, outcome)
    }

    /// Consume the full reservation after an error where partial native access
    /// cannot be measured exactly.
    pub fn fail_conservative(mut self) {
        self.finished = true;
        let _ = self.finish(
            self.request.max_bytes,
            self.request.max_rows,
            AccessOutcome::Failed,
        );
    }

    fn finish(
        &self,
        bytes_read: u64,
        rows_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        let actual_exceeds =
            bytes_read > self.request.max_bytes || rows_read > self.request.max_rows;
        let (accounted_bytes, accounted_rows, accounted_outcome, denied_limit) = if actual_exceeds {
            (
                self.request.max_bytes,
                self.request.max_rows,
                AccessOutcome::Failed,
                Some(AccessLimit::Reservation),
            )
        } else {
            (bytes_read, rows_read, outcome, None)
        };
        let mut state = self.inner.lock_state();
        release_count(&mut state.reserved_objects, self.request.object_token);
        release_count(
            &mut state.reserved_edges,
            (self.request.parent_token, self.request.object_token),
        );
        state.bytes_reserved = state.bytes_reserved.saturating_sub(self.request.max_bytes);
        state.rows_reserved = state.rows_reserved.saturating_sub(self.request.max_rows);
        state.committed_objects.insert(self.request.object_token);
        state
            .committed_edges
            .insert((self.request.parent_token, self.request.object_token));
        state.bytes_read = state.bytes_read.saturating_add(accounted_bytes);
        state.rows_read = state.rows_read.saturating_add(accounted_rows);
        state.max_depth_observed = state.max_depth_observed.max(self.request.depth);
        state.completed = state.completed.saturating_add(1);
        self.inner.push_trace(
            &mut state,
            AccessTraceEntry {
                access_trace_contract_version: ACCESS_TRACE_CONTRACT_VERSION,
                sequence: self.sequence,
                relation_id: self.inner.relation_id.clone(),
                operation: self.request.operation,
                phase: self.request.phase,
                parent_token: self.request.parent_token,
                object_token: self.request.object_token,
                depth: self.request.depth,
                reserved_bytes: self.request.max_bytes,
                reserved_rows: self.request.max_rows,
                bytes_read: accounted_bytes,
                rows_read: accounted_rows,
                outcome: accounted_outcome,
                denied_limit,
            },
        );
        if actual_exceeds {
            state.denied = state.denied.saturating_add(1);
            return Err(AccessBudgetError::ActualExceedsReservation);
        }
        Ok(())
    }
}

impl Drop for AccessReservation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut state = self.inner.lock_state();
        release_count(&mut state.reserved_objects, self.request.object_token);
        release_count(
            &mut state.reserved_edges,
            (self.request.parent_token, self.request.object_token),
        );
        state.bytes_reserved = state.bytes_reserved.saturating_sub(self.request.max_bytes);
        state.rows_reserved = state.rows_reserved.saturating_sub(self.request.max_rows);
        state.committed_objects.insert(self.request.object_token);
        state
            .committed_edges
            .insert((self.request.parent_token, self.request.object_token));
        state.bytes_read = state.bytes_read.saturating_add(self.request.max_bytes);
        state.rows_read = state.rows_read.saturating_add(self.request.max_rows);
        state.max_depth_observed = state.max_depth_observed.max(self.request.depth);
        state.completed = state.completed.saturating_add(1);
        state.abandoned = state.abandoned.saturating_add(1);
        self.inner.push_trace(
            &mut state,
            AccessTraceEntry {
                access_trace_contract_version: ACCESS_TRACE_CONTRACT_VERSION,
                sequence: self.sequence,
                relation_id: self.inner.relation_id.clone(),
                operation: self.request.operation,
                phase: self.request.phase,
                parent_token: self.request.parent_token,
                object_token: self.request.object_token,
                depth: self.request.depth,
                reserved_bytes: self.request.max_bytes,
                reserved_rows: self.request.max_rows,
                bytes_read: self.request.max_bytes,
                rows_read: self.request.max_rows,
                outcome: AccessOutcome::Abandoned,
                denied_limit: None,
            },
        );
    }
}

fn release_count<K: Ord + Copy>(counts: &mut BTreeMap<K, u64>, key: K) {
    let Some(count) = counts.get_mut(&key) else {
        return;
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        counts.remove(&key);
    }
}

fn sum_exceeds<const N: usize>(limit: u64, values: [u64; N]) -> bool {
    let mut total = 0_u64;
    for value in values {
        let Some(next) = total.checked_add(value) else {
            return true;
        };
        total = next;
    }
    total > limit
}

fn validate_relation_id(value: &str) -> Result<(), AccessBudgetError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_RELATION_ID_BYTES
        && value.trim() == value
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if !valid {
        return Err(AccessBudgetError::InvalidConfig(
            "relation id must match [a-z0-9][a-z0-9._-]{0,127}".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct SharedAccessFixture {
        fixture_contract_version: u32,
        relation_id: String,
        bounds: ScopeAccessBounds,
        operations: Vec<SharedAccessOperation>,
        denied_operation: SharedDeniedOperation,
        expected: SharedAccessExpected,
    }

    #[derive(Deserialize)]
    struct SharedAccessOperation {
        object_identity: String,
        depth: u32,
        max_bytes: u64,
        max_rows: u64,
        bytes_read: u64,
        rows_read: u64,
        outcome: AccessOutcome,
    }

    #[derive(Deserialize)]
    struct SharedDeniedOperation {
        object_identity: String,
        depth: u32,
        max_bytes: u64,
        max_rows: u64,
        expected_limit: AccessLimit,
    }

    #[derive(Deserialize)]
    struct SharedAccessExpected {
        attempts: u64,
        reservations_granted: u64,
        completed: u64,
        denied: u64,
        objects_accessed: u64,
        bytes_read: u64,
        rows_read: u64,
        max_depth_observed: u32,
        bytes_reserved: u64,
        rows_reserved: u64,
    }

    fn shared_fixture() -> SharedAccessFixture {
        serde_json::from_str(include_str!(
            "../../fixtures/contracts/rfc012a-access-v1.json"
        ))
        .unwrap()
    }

    fn bounds() -> ScopeAccessBounds {
        ScopeAccessBounds {
            max_fan_out: 2,
            max_depth: 3,
            max_objects: 3,
            max_bytes: 100,
            max_rows: 4,
        }
    }

    fn token(value: &[u8]) -> AccessObjectToken {
        AccessObjectToken::derive("descendants", &[value]).unwrap()
    }

    #[test]
    fn rust_access_budget_matches_the_independent_shared_fixture() {
        let fixture = shared_fixture();
        assert_eq!(fixture.fixture_contract_version, 1);
        let budget = AccessBudget::new(&fixture.relation_id, fixture.bounds).unwrap();
        for operation in fixture.operations {
            let object_token = AccessObjectToken::derive(
                &fixture.relation_id,
                &[operation.object_identity.as_bytes()],
            )
            .unwrap();
            budget
                .reserve(AccessReservationRequest {
                    operation: AccessOperation::ObjectRead,
                    phase: AccessPhase::Initial,
                    parent_token: None,
                    object_token,
                    depth: operation.depth,
                    max_bytes: operation.max_bytes,
                    max_rows: operation.max_rows,
                })
                .unwrap()
                .complete(operation.bytes_read, operation.rows_read, operation.outcome)
                .unwrap();
        }
        let denied = fixture.denied_operation;
        let denied_token =
            AccessObjectToken::derive(&fixture.relation_id, &[denied.object_identity.as_bytes()])
                .unwrap();
        assert!(matches!(
            budget.reserve(AccessReservationRequest {
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                object_token: denied_token,
                depth: denied.depth,
                max_bytes: denied.max_bytes,
                max_rows: denied.max_rows,
            }),
            Err(AccessBudgetError::LimitExceeded { limit, .. }) if limit == denied.expected_limit
        ));
        let actual = budget.snapshot();
        let expected = fixture.expected;
        assert_eq!(actual.attempts, expected.attempts);
        assert_eq!(actual.reservations_granted, expected.reservations_granted);
        assert_eq!(actual.completed, expected.completed);
        assert_eq!(actual.denied, expected.denied);
        assert_eq!(actual.objects_accessed, expected.objects_accessed);
        assert_eq!(actual.bytes_read, expected.bytes_read);
        assert_eq!(actual.rows_read, expected.rows_read);
        assert_eq!(actual.max_depth_observed, expected.max_depth_observed);
        assert_eq!(actual.bytes_reserved, expected.bytes_reserved);
        assert_eq!(actual.rows_reserved, expected.rows_reserved);
    }

    fn request(
        parent_token: Option<AccessObjectToken>,
        object_token: AccessObjectToken,
        max_bytes: u64,
        max_rows: u64,
    ) -> AccessReservationRequest {
        AccessReservationRequest {
            operation: AccessOperation::ObjectRead,
            phase: AccessPhase::Initial,
            parent_token,
            object_token,
            depth: 2,
            max_bytes,
            max_rows,
        }
    }

    #[test]
    fn reserves_before_access_and_accounts_actual_totals() {
        let budget = AccessBudget::new("descendants", bounds()).unwrap();
        budget
            .reserve(request(None, token(b"a"), 60, 2))
            .unwrap()
            .complete(40, 1, AccessOutcome::Available)
            .unwrap();
        budget
            .reserve(request(None, token(b"b"), 60, 3))
            .unwrap()
            .complete(60, 3, AccessOutcome::Available)
            .unwrap();

        let snapshot = budget.snapshot();
        assert_eq!(snapshot.objects_accessed, 2);
        assert_eq!(snapshot.bytes_read, 100);
        assert_eq!(snapshot.rows_read, 4);
        assert_eq!(snapshot.max_depth_observed, 2);
        assert_eq!(snapshot.bytes_reserved, 0);
        assert_eq!(snapshot.rows_reserved, 0);
        assert_eq!(snapshot.trace.len(), 2);
    }

    #[test]
    fn denial_is_pre_access_and_does_not_mutate_committed_totals() {
        let budget = AccessBudget::new("descendants", bounds()).unwrap();
        let error = match budget.reserve(request(None, token(b"too-large"), 101, 0)) {
            Ok(_) => panic!("oversized reservation unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            AccessBudgetError::LimitExceeded {
                relation_id: "descendants".to_string(),
                limit: AccessLimit::MaxBytes,
            }
        );
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.denied, 1);
        assert_eq!(snapshot.objects_accessed, 0);
        assert_eq!(snapshot.bytes_read, 0);
        assert_eq!(snapshot.trace[0].outcome, AccessOutcome::Denied);
    }

    #[test]
    fn fan_out_is_per_parent_and_repeated_objects_do_not_consume_it_twice() {
        let budget = AccessBudget::new("descendants", bounds()).unwrap();
        let parent_a = token(b"parent-a");
        let parent_b = token(b"parent-b");
        for child in [token(b"child-a"), token(b"child-b")] {
            budget
                .reserve(request(Some(parent_a), child, 1, 0))
                .unwrap()
                .complete(1, 0, AccessOutcome::Available)
                .unwrap();
        }
        budget
            .reserve(request(Some(parent_a), token(b"child-a"), 1, 0))
            .unwrap()
            .complete(1, 0, AccessOutcome::Available)
            .unwrap();
        budget
            .reserve(request(Some(parent_b), token(b"child-a"), 1, 0))
            .unwrap()
            .complete(1, 0, AccessOutcome::Available)
            .unwrap();
        assert!(matches!(
            budget.reserve(request(Some(parent_a), token(b"child-c"), 1, 0)),
            Err(AccessBudgetError::LimitExceeded {
                limit: AccessLimit::MaxFanOut,
                ..
            })
        ));
    }

    #[test]
    fn overlapping_repeated_reservations_count_one_object_and_edge() {
        let budget = AccessBudget::new(
            "one-object",
            ScopeAccessBounds {
                max_fan_out: 1,
                max_depth: 1,
                max_objects: 1,
                max_bytes: 10,
                max_rows: 0,
            },
        )
        .unwrap();
        let object = AccessObjectToken::derive("one-object", &[b"same"]).unwrap();
        let request = AccessReservationRequest {
            operation: AccessOperation::ObjectRead,
            phase: AccessPhase::Initial,
            parent_token: None,
            object_token: object,
            depth: 1,
            max_bytes: 4,
            max_rows: 0,
        };
        let first = budget.reserve(request).unwrap();
        let second = budget.reserve(request).unwrap();
        first.complete(2, 0, AccessOutcome::Available).unwrap();
        second.complete(2, 0, AccessOutcome::Available).unwrap();
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.objects_accessed, 1);
        assert_eq!(snapshot.bytes_read, 4);
    }

    #[test]
    fn outstanding_reservations_cannot_overcommit_and_abandonment_is_conservative() {
        let budget = AccessBudget::new("descendants", bounds()).unwrap();
        let first = budget.reserve(request(None, token(b"a"), 75, 2)).unwrap();
        assert!(matches!(
            budget.reserve(request(None, token(b"b"), 26, 0)),
            Err(AccessBudgetError::LimitExceeded {
                limit: AccessLimit::MaxBytes,
                ..
            })
        ));
        drop(first);
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.abandoned, 1);
        assert_eq!(snapshot.bytes_read, 75);
        assert_eq!(snapshot.rows_read, 2);
        assert!(snapshot
            .trace
            .iter()
            .any(|entry| entry.outcome == AccessOutcome::Abandoned));
    }

    #[test]
    fn trace_is_bounded_and_actual_values_cannot_exceed_reservation() {
        let budget = AccessBudget::with_trace_capacity("descendants", bounds(), 2).unwrap();
        let error = budget
            .reserve(request(None, token(b"a"), 1, 0))
            .unwrap()
            .complete(2, 0, AccessOutcome::Available)
            .unwrap_err();
        assert_eq!(error, AccessBudgetError::ActualExceedsReservation);
        for value in [b"b".as_slice(), b"b".as_slice()] {
            budget
                .reserve(request(None, token(value), 1, 0))
                .unwrap()
                .complete(1, 0, AccessOutcome::Available)
                .unwrap();
        }
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.trace.len(), 2);
        assert_eq!(snapshot.trace_entries_dropped, 1);
        assert_eq!(snapshot.denied, 1);
    }
}
