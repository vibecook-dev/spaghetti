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
    AccessOperation, AccessOutcome, AccessPhase, AuthorizedScopeAccessPlan, Revision,
    ScopeAccessReport, ScopeAccessRequest, ScopeIdentityInput, SourceDriverError, StableRead,
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
