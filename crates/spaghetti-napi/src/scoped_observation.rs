//! Database-free RFC 012 scoped-observation composition root.
//!
//! This module owns the seam between the strict adapter/support registry and
//! common source access. It deliberately exposes no N-API surface yet: native
//! artifact probing and the complete RFC 012D request contract must remain a
//! trusted Rust-host concern until their portable contracts are frozen.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::adapter::{
    AdapterId, AdapterRegistry, CompatibilityDecision, ContractVersionOffer,
    ContractVersionRequest, NativeArtifactProbe, ScopeRelationPrimitive, SupportOperation,
    TypedAccessAuthorization,
};
use crate::source::{
    confined_relative_path_key, read_stable_file_confined, AccessBudgetError, AccessObjectToken,
    AccessOperation, AccessOutcome, AccessPhase, AppendCheckpoint, AppendDelimitedFile, AppendRead,
    AppendTransition, AuthorizedScopeAccessPlan, RecordOrigin, Revision, ScopeAccessReport,
    ScopeAccessRequest, ScopeIdentityInput, SourceDriverError, StableRead,
};

/// One exact host-approved object locator. The locator is installed during
/// attachment and cannot be replaced by a decoder or by an access call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedKnownObjectGrant {
    pub relation_id: String,
    pub access_root: String,
    pub locator_id: String,
    pub root: PathBuf,
    pub relative_path: PathBuf,
}

/// Internal attachment request. The artifact probe is supplied by the trusted
/// Rust composition root, never by a portable runtime consumer.
#[derive(Debug, Clone)]
pub struct ScopedObservationAccessRequest {
    pub adapter_id: String,
    pub artifact_probe: NativeArtifactProbe,
    pub contract_request: ContractVersionRequest,
    pub contract_offer: ContractVersionOffer,
    pub program_id: String,
    pub known_objects: Vec<ScopedKnownObjectGrant>,
}

/// One read against an exact known-object grant. Native identity bytes are
/// consumed only to validate the declaration and derive the opaque audit token.
#[derive(Debug, Clone, Copy)]
pub struct ScopedKnownObjectReadRequest<'a> {
    pub relation_id: &'a str,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub depth: u32,
    pub max_bytes: u64,
}

/// One bounded append-driver invocation against an exact known-object grant.
#[derive(Debug, Clone, Copy)]
pub struct ScopedKnownAppendReadRequest<'a> {
    pub relation_id: &'a str,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub depth: u32,
    pub max_bytes: u64,
    pub previous: Option<&'a AppendCheckpoint>,
    pub origin: &'a RecordOrigin,
    pub force_contract_replay: bool,
}

/// Stateful root append reconciliation input. The checkpoint is intentionally
/// absent: the store-free kernel owns it and cannot accept a caller substitute.
#[derive(Debug, Clone, Copy)]
pub struct ScopedAppendReconcileRequest<'a> {
    pub relation_id: &'a str,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub access_phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub depth: u32,
    pub max_bytes: u64,
    pub origin: &'a RecordOrigin,
    pub force_contract_replay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedAppendDeliveryPhase {
    Bootstrap,
    Live,
    Correction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedAppendReset {
    pub old_generation: u64,
    pub new_generation: u64,
    pub reason: AppendTransition,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ScopedAppendObservation {
    admission_token: u64,
    pub phase: ScopedAppendDeliveryPhase,
    pub reset_before_items: Option<ScopedAppendReset>,
    pub root_present: bool,
    pub became_missing: bool,
    pub read: AppendRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedObjectRead {
    Available { bytes: Vec<u8>, revision: Revision },
    Unavailable,
    Oversized { observed_bytes: u64 },
    Unstable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedSourceFailureClass {
    InvalidConfiguration,
    InvalidCursor,
    PathEscape,
    LimitExceeded,
    Unstable,
    Database,
    Io,
}

#[derive(Debug, thiserror::Error)]
pub enum ScopedObservationAccessError {
    #[error("scoped observation authorization failed: {0}")]
    Authorization(String),
    #[error("invalid scoped access grant: {0}")]
    InvalidGrant(String),
    #[error("scoped observation host is closed")]
    Closed,
    #[error("a scoped access pass is already active")]
    PassAlreadyActive,
    #[error("scoped append bootstrap has not reached a drainable observation")]
    BootstrapNotDrained,
    #[error("scoped append bootstrap is already complete")]
    BootstrapAlreadyComplete,
    #[error("a scoped append observation is awaiting admission or discard")]
    ObservationPending,
    #[error("the scoped append observation does not match the pending admission")]
    ObservationNotPending,
    #[error("the scoped append admission sequence is exhausted")]
    ObservationSequenceExhausted,
    #[error("scoped source access failed: {0:?}")]
    Source(ScopedSourceFailureClass),
    #[error(transparent)]
    Access(#[from] AccessBudgetError),
}

struct ScopedObservationAccessState {
    closed: AtomicBool,
    pass_active: AtomicBool,
}

/// The database-free scoped composition root owns the unforgeable typed
/// authorization and exact grants. It creates a fresh access ledger only at a
/// common-runtime pass boundary and never exposes the authorization itself.
pub struct ScopedObservationAccessHost {
    compatibility: CompatibilityDecision,
    authorization: TypedAccessAuthorization,
    program_id: String,
    known_objects: Arc<BTreeMap<String, ScopedKnownObjectGrant>>,
    state: Arc<ScopedObservationAccessState>,
}

impl ScopedObservationAccessHost {
    pub fn authorize(
        registry: &AdapterRegistry,
        request: ScopedObservationAccessRequest,
    ) -> Result<Self, ScopedObservationAccessError> {
        let adapter_id = AdapterId::new(request.adapter_id.as_str())
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let (compatibility, authorization) = registry
            .authorize_typed_access(
                &adapter_id,
                &request.artifact_probe,
                SupportOperation::ScopedTypedObservation,
                &request.contract_request,
                &request.contract_offer,
            )
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let program = authorization
            .select_scope_program(&request.program_id)
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))?;
        let plan = AuthorizedScopeAccessPlan::from_authorized_program(program)?;
        let known_objects = validate_known_object_grants(&plan, request.known_objects)?;
        Ok(Self {
            compatibility,
            authorization,
            program_id: request.program_id,
            known_objects: Arc::new(known_objects),
            state: Arc::new(ScopedObservationAccessState {
                closed: AtomicBool::new(false),
                pass_active: AtomicBool::new(false),
            }),
        })
    }

    pub fn compatibility(&self) -> &CompatibilityDecision {
        &self.compatibility
    }

    pub fn begin_pass(&self) -> Result<ScopedObservationAccessPass, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        self.state
            .pass_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ScopedObservationAccessError::PassAlreadyActive)?;
        if self.state.closed.load(Ordering::Acquire) {
            self.state.pass_active.store(false, Ordering::Release);
            return Err(ScopedObservationAccessError::Closed);
        }
        let plan = match self
            .authorization
            .select_scope_program(&self.program_id)
            .map_err(|error| ScopedObservationAccessError::Authorization(error.to_string()))
            .and_then(|program| {
                AuthorizedScopeAccessPlan::from_authorized_program(program).map_err(Into::into)
            }) {
            Ok(plan) => plan,
            Err(error) => {
                self.state.pass_active.store(false, Ordering::Release);
                return Err(error);
            }
        };
        Ok(ScopedObservationAccessPass {
            plan,
            known_objects: Arc::clone(&self.known_objects),
            state: Arc::clone(&self.state),
            released: false,
        })
    }

    /// Idempotently prevent new passes. A pass read that observes closure after
    /// its operating-system call is accounted conservatively before returning.
    pub fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
    }

    pub fn is_closed(&self) -> bool {
        self.state.closed.load(Ordering::Acquire)
    }
}

impl Drop for ScopedObservationAccessHost {
    fn drop(&mut self) {
        self.close();
    }
}

/// One bounded reconciliation pass. Dropping the pass releases the host's
/// single-pass slot; a later pass receives a fresh declaration-sized ledger.
pub struct ScopedObservationAccessPass {
    plan: AuthorizedScopeAccessPlan,
    known_objects: Arc<BTreeMap<String, ScopedKnownObjectGrant>>,
    state: Arc<ScopedObservationAccessState>,
    released: bool,
}

impl ScopedObservationAccessPass {
    pub fn read_known_object(
        &self,
        request: ScopedKnownObjectReadRequest<'_>,
    ) -> Result<ScopedObjectRead, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        let grant = self.known_objects.get(request.relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} has no exact known-object grant",
                request.relation_id
            ))
        })?;
        let max_bytes = usize::try_from(request.max_bytes).map_err(|_| {
            ScopedObservationAccessError::InvalidGrant(
                "known-object byte reservation exceeds this platform".to_string(),
            )
        })?;
        let reservation = self.plan.reserve(ScopeAccessRequest {
            relation_id: request.relation_id,
            operation: AccessOperation::ObjectRead,
            phase: request.phase,
            parent_token: request.parent_token,
            identity_inputs: request.identity_inputs,
            depth: request.depth,
            max_bytes: request.max_bytes,
            max_rows: 0,
        })?;
        if reservation.primitive() != ScopeRelationPrimitive::KnownObject
            || reservation.access_root() != grant.access_root
            || reservation.locator() != grant.locator_id
        {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::InvalidGrant(
                "known-object grant no longer matches its authorized declaration".to_string(),
            ));
        }

        let read = match read_stable_file_confined(&grant.root, &grant.relative_path, max_bytes) {
            Ok(read) => read,
            Err(error) => {
                reservation.fail_conservative();
                return Err(ScopedObservationAccessError::Source(source_failure_class(
                    &error,
                )));
            }
        };
        if self.state.closed.load(Ordering::Acquire) {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::Closed);
        }
        match read {
            StableRead::Missing => {
                reservation.complete(0, 0, AccessOutcome::Unavailable)?;
                Ok(ScopedObjectRead::Unavailable)
            }
            StableRead::Oversized(stamp) => {
                reservation.complete(0, 0, AccessOutcome::Oversized)?;
                Ok(ScopedObjectRead::Oversized {
                    observed_bytes: stamp.len,
                })
            }
            StableRead::Unstable => {
                reservation.fail_conservative();
                Ok(ScopedObjectRead::Unstable)
            }
            StableRead::Stable {
                bytes, revision, ..
            } => {
                reservation.complete(bytes.len() as u64, 0, AccessOutcome::Available)?;
                Ok(ScopedObjectRead::Available { bytes, revision })
            }
        }
    }

    pub fn read_known_append(
        &self,
        driver: &AppendDelimitedFile,
        request: ScopedKnownAppendReadRequest<'_>,
    ) -> Result<AppendRead, ScopedObservationAccessError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationAccessError::Closed);
        }
        let grant = self.known_objects.get(request.relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} has no exact known-object grant",
                request.relation_id
            ))
        })?;
        let reservation = self.plan.reserve(ScopeAccessRequest {
            relation_id: request.relation_id,
            operation: AccessOperation::ObjectRead,
            phase: request.phase,
            parent_token: request.parent_token,
            identity_inputs: request.identity_inputs,
            depth: request.depth,
            max_bytes: request.max_bytes,
            max_rows: 0,
        })?;
        if reservation.primitive() != ScopeRelationPrimitive::KnownObject
            || reservation.access_root() != grant.access_root
            || reservation.locator() != grant.locator_id
        {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::InvalidGrant(
                "known-object grant no longer matches its authorized declaration".to_string(),
            ));
        }

        let read = match driver.read_confined_bounded(
            &grant.root,
            &grant.relative_path,
            request.previous,
            request.origin,
            request.force_contract_replay,
            request.max_bytes,
        ) {
            Ok(read) => read,
            Err(error) => {
                reservation.fail_conservative();
                return Err(ScopedObservationAccessError::Source(source_failure_class(
                    &error,
                )));
            }
        };
        if self.state.closed.load(Ordering::Acquire) {
            reservation.fail_conservative();
            return Err(ScopedObservationAccessError::Closed);
        }
        match &read {
            AppendRead::Missing => reservation.complete(0, 0, AccessOutcome::Unavailable)?,
            AppendRead::RetryTransient => reservation.fail_conservative(),
            AppendRead::Batch { bytes_read, .. } => {
                reservation.complete(*bytes_read, 0, AccessOutcome::Available)?;
            }
        }
        Ok(read)
    }

    pub fn report(&self) -> ScopeAccessReport {
        self.plan.report()
    }

    pub fn finish(mut self) -> ScopeAccessReport {
        let report = self.plan.report();
        self.release();
        report
    }

    fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.state.pass_active.store(false, Ordering::Release);
        }
    }
}

impl Drop for ScopedObservationAccessPass {
    fn drop(&mut self) {
        self.release();
    }
}

/// In-memory cursor/generation state for one exact append-delimited root. It
/// does not own a store, query service, watcher, or public event queue.
pub struct ScopedKnownAppendObject {
    driver: AppendDelimitedFile,
    checkpoint: Option<AppendCheckpoint>,
    bootstrap_active: bool,
    bootstrap_observed: bool,
    bootstrap_blocked: bool,
    root_present: bool,
    next_admission_token: u64,
    pending: Option<PendingAppendState>,
}

struct PendingAppendState {
    admission_token: u64,
    checkpoint: Option<AppendCheckpoint>,
    bootstrap_blocked: bool,
    root_present: bool,
}

impl ScopedKnownAppendObject {
    pub fn new(driver: AppendDelimitedFile) -> Self {
        Self {
            driver,
            checkpoint: None,
            bootstrap_active: true,
            bootstrap_observed: false,
            bootstrap_blocked: false,
            root_present: false,
            next_admission_token: 1,
            pending: None,
        }
    }

    pub fn reconcile(
        &mut self,
        pass: &ScopedObservationAccessPass,
        request: ScopedAppendReconcileRequest<'_>,
    ) -> Result<ScopedAppendObservation, ScopedObservationAccessError> {
        if self.pending.is_some() {
            return Err(ScopedObservationAccessError::ObservationPending);
        }
        let previous_generation = self.checkpoint.as_ref().map(|value| value.generation);
        let read = pass.read_known_append(
            &self.driver,
            ScopedKnownAppendReadRequest {
                relation_id: request.relation_id,
                identity_inputs: request.identity_inputs,
                phase: request.access_phase,
                parent_token: request.parent_token,
                depth: request.depth,
                max_bytes: request.max_bytes,
                previous: self.checkpoint.as_ref(),
                origin: request.origin,
                force_contract_replay: request.force_contract_replay,
            },
        )?;
        let (
            reset_before_items,
            became_missing,
            next_checkpoint,
            next_root_present,
            next_bootstrap_blocked,
        ) = match &read {
            AppendRead::Missing => {
                let became_missing = self.root_present;
                (None, became_missing, self.checkpoint.clone(), false, false)
            }
            AppendRead::RetryTransient => (
                None,
                false,
                self.checkpoint.clone(),
                self.root_present,
                true,
            ),
            AppendRead::Batch {
                checkpoint,
                transition,
                more_available,
                ..
            } => {
                let reset = if matches!(
                    transition,
                    AppendTransition::Truncated
                        | AppendTransition::IdentityChanged
                        | AppendTransition::PrefixMismatch
                        | AppendTransition::ContractReplay
                ) {
                    Some(ScopedAppendReset {
                        old_generation: previous_generation.expect(
                            "a reset transition is possible only with a previous checkpoint",
                        ),
                        new_generation: checkpoint.generation,
                        reason: *transition,
                    })
                } else {
                    None
                };
                (
                    reset,
                    false,
                    Some(checkpoint.clone()),
                    true,
                    *more_available,
                )
            }
        };
        let phase = if reset_before_items.is_some() {
            ScopedAppendDeliveryPhase::Correction
        } else if self.bootstrap_active {
            ScopedAppendDeliveryPhase::Bootstrap
        } else {
            ScopedAppendDeliveryPhase::Live
        };
        let admission_token = self.next_admission_token;
        self.next_admission_token = self
            .next_admission_token
            .checked_add(1)
            .ok_or(ScopedObservationAccessError::ObservationSequenceExhausted)?;
        self.pending = Some(PendingAppendState {
            admission_token,
            checkpoint: next_checkpoint,
            bootstrap_blocked: next_bootstrap_blocked,
            root_present: next_root_present,
        });
        Ok(ScopedAppendObservation {
            admission_token,
            phase,
            reset_before_items,
            root_present: next_root_present,
            became_missing,
            read,
        })
    }

    /// Advance the cursor only after the observation's records and any reset
    /// control have been admitted to the ordered observer lane.
    pub fn admit(
        &mut self,
        observation: &ScopedAppendObservation,
    ) -> Result<(), ScopedObservationAccessError> {
        let Some(pending) = self.pending.take() else {
            return Err(ScopedObservationAccessError::ObservationNotPending);
        };
        if pending.admission_token != observation.admission_token {
            self.pending = Some(pending);
            return Err(ScopedObservationAccessError::ObservationNotPending);
        }
        self.checkpoint = pending.checkpoint;
        self.bootstrap_blocked = pending.bootstrap_blocked;
        self.root_present = pending.root_present;
        self.bootstrap_observed = true;
        Ok(())
    }

    /// Discard a read that could not be admitted. Its native access remains in
    /// the pass report, but the cursor does not advance and the next pass
    /// deterministically replays it.
    pub fn discard(
        &mut self,
        observation: &ScopedAppendObservation,
    ) -> Result<(), ScopedObservationAccessError> {
        let Some(pending) = self.pending.take() else {
            return Err(ScopedObservationAccessError::ObservationNotPending);
        };
        if pending.admission_token != observation.admission_token {
            self.pending = Some(pending);
            return Err(ScopedObservationAccessError::ObservationNotPending);
        }
        Ok(())
    }

    /// Close the bootstrap admission phase after at least one stable missing
    /// or fully drained batch observation. An incomplete final record is safe:
    /// its checkpoint retains the suffix for the next live reconciliation.
    pub fn complete_bootstrap(&mut self) -> Result<(), ScopedObservationAccessError> {
        if !self.bootstrap_active {
            return Err(ScopedObservationAccessError::BootstrapAlreadyComplete);
        }
        if self.pending.is_some() || !self.bootstrap_observed || self.bootstrap_blocked {
            return Err(ScopedObservationAccessError::BootstrapNotDrained);
        }
        self.bootstrap_active = false;
        Ok(())
    }

    pub fn checkpoint(&self) -> Option<&AppendCheckpoint> {
        self.checkpoint.as_ref()
    }

    pub fn root_present(&self) -> bool {
        self.root_present
    }

    pub fn bootstrap_active(&self) -> bool {
        self.bootstrap_active
    }
}

fn validate_known_object_grants(
    plan: &AuthorizedScopeAccessPlan,
    grants: Vec<ScopedKnownObjectGrant>,
) -> Result<BTreeMap<String, ScopedKnownObjectGrant>, ScopedObservationAccessError> {
    if grants.is_empty() {
        return Err(ScopedObservationAccessError::InvalidGrant(
            "at least one exact known-object grant is required".to_string(),
        ));
    }
    let mut validated = BTreeMap::new();
    for grant in grants {
        if grant.root.as_os_str().is_empty() || !grant.root.is_absolute() {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} requires an absolute host-approved root",
                grant.relation_id
            )));
        }
        confined_relative_path_key(&grant.relative_path).map_err(|_| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} has a non-confined relative locator",
                grant.relation_id
            ))
        })?;
        let relation = plan.relation(&grant.relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} is absent from the authorized program",
                grant.relation_id
            ))
        })?;
        if relation.primitive != ScopeRelationPrimitive::KnownObject
            || relation.access_root != grant.access_root
            || relation.locator != grant.locator_id
        {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} does not match an exact authorized known object",
                grant.relation_id
            )));
        }
        let relation_id = grant.relation_id.clone();
        if validated.insert(relation_id.clone(), grant).is_some() {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "duplicate known-object grant for relation {relation_id:?}"
            )));
        }
    }
    Ok(validated)
}

fn source_failure_class(error: &SourceDriverError) -> ScopedSourceFailureClass {
    match error {
        SourceDriverError::InvalidConfig(_) => ScopedSourceFailureClass::InvalidConfiguration,
        SourceDriverError::InvalidCursor(_) => ScopedSourceFailureClass::InvalidCursor,
        SourceDriverError::PathEscape(_) => ScopedSourceFailureClass::PathEscape,
        SourceDriverError::LimitExceeded(_) => ScopedSourceFailureClass::LimitExceeded,
        SourceDriverError::Unstable(_) => ScopedSourceFailureClass::Unstable,
        SourceDriverError::Database(_) => ScopedSourceFailureClass::Database,
        SourceDriverError::Io { .. } => ScopedSourceFailureClass::Io,
    }
}
