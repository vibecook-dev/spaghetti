//! Agent-neutral scope/dependency access budgets and bounded telemetry.
//!
//! A source or observer runtime reserves the declared worst-case access before
//! touching a native object, then commits the actual bytes and rows. Failed or
//! abandoned reservations consume their full reservation conservatively. This
//! prevents retries, panics, and partial reads from bypassing a scope budget.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adapter::{
    AgentAdapter, AuthorizedObservationSourceAuthority, AuthorizedObservationSourceContract,
    AuthorizedObservationSourceDriver, AuthorizedScopeProgram, ConsistencyPolicy, DeletionPolicy,
    DriverSpec, ScopeObservationSourceBinding, ScopeProgramManifest, ScopeProgramStatus,
    ScopeRelationBounds, ScopeRelationDeclaration, ScopeRelationPrimitive,
    ScopeUnavailableBehavior, SourceInstance, SourceInstanceKey, StreamAuthority, StreamSpec,
};

pub const ACCESS_TRACE_CONTRACT_VERSION: u32 = 1;
pub const SCOPE_ACCESS_REPORT_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_ACCESS_TRACE_CAPACITY: usize = 256;

const MAX_RELATION_ID_BYTES: usize = 128;
const MAX_TRACE_CAPACITY: usize = 16_384;
const MAX_RENDERED_SCOPE_LOCATOR_BYTES: usize = 4 * 1024;
const DIRECTORY_ENTRY_TOKEN_DOMAIN: &[u8] = b"spaghetti/rfc012a/directory-enumeration-entry/v1\0";
pub(crate) const MAX_IDENTITY_VALUE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        if self.max_fan_out > self.max_objects {
            return Err(AccessBudgetError::InvalidConfig(
                "access max_fan_out cannot exceed max_objects".to_string(),
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    #[error("scope access denied for program {program_id}, relation {relation_id}: {reason:?}")]
    ScopeDenied {
        program_id: String,
        relation_id: String,
        reason: ScopeAccessDenial,
    },
    #[error("actual access exceeded its reservation")]
    ActualExceedsReservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeAccessDenial {
    UnknownRelation,
    OperationNotAllowed,
    IdentityInputsMismatch,
    InvalidReservation,
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

/// One named native identity value supplied to a declarative scope relation.
/// The name is checked against the declaration before its value contributes to
/// the opaque object token.
#[derive(Debug, Clone, Copy)]
pub struct ScopeIdentityInput<'a> {
    pub name: &'a str,
    pub value: &'a [u8],
}

/// A bounded request against one relation in a compiled scope program.
/// Locators and access roots are intentionally not caller-controlled: a
/// successful reservation returns those values from the verified declaration.
#[derive(Debug, Clone, Copy)]
pub struct ScopeAccessRequest<'a> {
    pub relation_id: &'a str,
    pub operation: AccessOperation,
    pub phase: AccessPhase,
    pub parent_token: Option<AccessObjectToken>,
    pub identity_inputs: &'a [ScopeIdentityInput<'a>],
    pub depth: u32,
    pub max_bytes: u64,
    pub max_rows: u64,
}

/// Common-engine ledger for one program in one bounded reconciliation pass.
/// Clones share the same relation budgets and cannot refill a pass.
#[derive(Clone)]
pub struct ScopeAccessPlan {
    adapter_id: String,
    declaration_id: String,
    status: ScopeProgramStatus,
    program_id: String,
    root_relation_id: Option<String>,
    relations: BTreeMap<String, CompiledScopeRelation>,
}

/// One authorized, bounded reconciliation-pass ledger. It can only be created
/// from a program selected by Rust support authorization. Clones share the
/// same budgets and therefore cannot refill the pass.
#[derive(Clone)]
pub struct AuthorizedScopeAccessPlan {
    inner: ScopeAccessPlan,
    support_release_id: String,
    support_release_digest: [u8; 32],
    source_declaration_digest: [u8; 32],
    scope_program_digest: [u8; 32],
    selection_contract_version: u32,
    observation_contract_version: u32,
    observation_source_contracts: BTreeMap<String, AuthorizedObservationSourceContract>,
}

impl AuthorizedScopeAccessPlan {
    pub fn from_authorized_program(
        authorization: AuthorizedScopeProgram<'_>,
    ) -> Result<Self, AccessBudgetError> {
        let inner = ScopeAccessPlan::for_program(
            authorization.scope_programs(),
            authorization.program_id(),
        )?;
        if inner.status() != ScopeProgramStatus::Promoted {
            return Err(AccessBudgetError::InvalidConfig(
                "authorized scope access requires a promoted declaration".to_string(),
            ));
        }
        let mut observation_source_contracts = BTreeMap::new();
        for relation in inner
            .relations
            .values()
            .filter(|relation| relation.declaration.observation_binding.is_some())
        {
            let contract = authorization
                .observation_source_contract(&relation.declaration.relation_id)
                .cloned()
                .ok_or_else(invalid_observation_source_reservation)?;
            observation_source_contracts.insert(relation.declaration.relation_id.clone(), contract);
        }
        Ok(Self {
            inner,
            support_release_id: authorization.support_release_id().to_string(),
            support_release_digest: *authorization.support_release_digest().as_bytes(),
            source_declaration_digest: *authorization.source_declaration_digest().as_bytes(),
            scope_program_digest: *authorization.scope_program_digest().as_bytes(),
            selection_contract_version: authorization.selection_contract_version(),
            observation_contract_version: authorization.observation_contract_version(),
            observation_source_contracts,
        })
    }

    pub fn adapter_id(&self) -> &str {
        self.inner.adapter_id()
    }

    pub fn declaration_id(&self) -> &str {
        self.inner.declaration_id()
    }

    pub fn program_id(&self) -> &str {
        self.inner.program_id()
    }

    pub fn support_release_id(&self) -> &str {
        &self.support_release_id
    }

    pub(crate) fn source_declaration_digest(&self) -> &[u8; 32] {
        &self.source_declaration_digest
    }

    pub fn relation(&self, relation_id: &str) -> Option<&ScopeRelationDeclaration> {
        self.inner.relation(relation_id)
    }

    pub(crate) fn root_relation_id(&self) -> &str {
        self.inner
            .root_relation_id
            .as_deref()
            .expect("promoted authorized scope programs declare a validated root relation")
    }

    pub(crate) fn known_object_relation_ids(&self) -> impl Iterator<Item = &str> {
        self.inner
            .relations
            .values()
            .filter(|relation| {
                relation.declaration.primitive == ScopeRelationPrimitive::KnownObject
            })
            .map(|relation| relation.declaration.relation_id.as_str())
    }

    /// Relation primitives whose source membership is not yet represented by
    /// the scoped observer's exact known-object coverage vector. Evidence-
    /// derived artifact relations are transported through their separate
    /// attachment-bound availability contract and therefore do not enter this
    /// gate.
    pub(crate) fn uncomposed_observation_relation_ids(&self) -> impl Iterator<Item = &str> {
        self.observation_relations()
            .filter(|relation| relation.primitive != ScopeRelationPrimitive::KnownObject)
            .map(|relation| relation.relation_id.as_str())
    }

    /// Every declared relation represented by RFC 012D scope coverage.
    /// Artifact availability has its own contextual replacement contract and
    /// is intentionally not folded into Decode membership.
    pub(crate) fn observation_relations(&self) -> impl Iterator<Item = &ScopeRelationDeclaration> {
        self.inner
            .relations
            .values()
            .map(|relation| &relation.declaration)
            .filter(|relation| {
                relation.primitive != ScopeRelationPrimitive::ArtifactLocatorFromEvidence
            })
    }

    /// Reserve one declaration-owned dynamic/related source coordinate from
    /// this exact typed support authorization. Unlike [`ScopeAccessPlan`]'s
    /// generic reservation, the returned value proves that the source stream,
    /// pattern, and scope/source/support digests came from the verified
    /// Promoted bundle; none is accepted from the caller.
    pub(crate) fn reserve_observation_source(
        &self,
        request: ScopeAccessRequest<'_>,
    ) -> Result<AuthorizedObservationSourceReservation, AccessBudgetError> {
        let declaration = self
            .inner
            .relation(request.relation_id)
            .ok_or_else(invalid_observation_source_reservation)?;
        if !matches!(
            declaration.primitive,
            ScopeRelationPrimitive::ChildDirectoryByNativeId
                | ScopeRelationPrimitive::SiblingObject
                | ScopeRelationPrimitive::ReferencedObjectFromField
        ) {
            return Err(invalid_observation_source_reservation());
        }
        let binding = declaration
            .observation_binding
            .clone()
            .ok_or_else(invalid_observation_source_reservation)?;
        let source_contract = self
            .observation_source_contracts
            .get(request.relation_id)
            .filter(|contract| {
                contract.stream_id() == binding.stream_id
                    && contract.root_id() == declaration.access_root
            })
            .cloned()
            .ok_or_else(invalid_observation_source_reservation)?;
        let reservation = self.inner.reserve(request)?;
        let locator = match reservation.primitive() {
            ScopeRelationPrimitive::ChildDirectoryByNativeId => {
                reservation.render_child_directory_locator(request.identity_inputs)
            }
            ScopeRelationPrimitive::SiblingObject
            | ScopeRelationPrimitive::ReferencedObjectFromField => {
                reservation.render_related_object_locator(request.identity_inputs)
            }
            _ => Err(invalid_observation_source_reservation()),
        };
        let locator = match locator {
            Ok(locator) => locator,
            Err(error) => {
                reservation.fail_conservative();
                return Err(error);
            }
        };
        Ok(AuthorizedObservationSourceReservation {
            reservation,
            adapter_id: self.adapter_id().to_string(),
            program_id: self.program_id().to_string(),
            support_release_id: self.support_release_id.clone(),
            binding,
            source_contract,
            locator,
            support_release_digest: self.support_release_digest,
            source_declaration_digest: self.source_declaration_digest,
            scope_program_digest: self.scope_program_digest,
        })
    }

    pub fn reserve(
        &self,
        request: ScopeAccessRequest<'_>,
    ) -> Result<ScopeAccessReservation, AccessBudgetError> {
        self.inner.reserve(request)
    }

    pub fn report(&self) -> ScopeAccessReport {
        ScopeAccessReport::new(self)
    }
}

#[derive(Clone)]
struct CompiledScopeRelation {
    declaration: ScopeRelationDeclaration,
    budget: AccessBudget,
}

impl ScopeAccessPlan {
    /// Compile one declaration program into common-engine relation budgets.
    /// Compilation is mechanical and does not itself grant native-source
    /// authority; the host must still carry a typed support authorization.
    pub fn for_program(
        manifest: &ScopeProgramManifest,
        program_id: &str,
    ) -> Result<Self, AccessBudgetError> {
        manifest.validate().map_err(|error| {
            AccessBudgetError::InvalidConfig(format!("invalid scope program: {error}"))
        })?;
        let program = manifest.program(program_id).ok_or_else(|| {
            AccessBudgetError::InvalidConfig(format!(
                "scope program {program_id:?} is not declared for adapter {}",
                manifest.adapter_id
            ))
        })?;
        let mut relations = BTreeMap::new();
        for declaration in &program.relations {
            let bounds = ScopeAccessBounds {
                max_fan_out: declaration.bounds.max_fan_out,
                max_depth: declaration.bounds.max_depth,
                max_objects: declaration.bounds.max_objects,
                max_bytes: declaration.bounds.max_bytes,
                max_rows: declaration.bounds.max_rows,
            };
            let budget = AccessBudget::new(&declaration.relation_id, bounds)?;
            relations.insert(
                declaration.relation_id.clone(),
                CompiledScopeRelation {
                    declaration: declaration.clone(),
                    budget,
                },
            );
        }
        Ok(Self {
            adapter_id: manifest.adapter_id.clone(),
            declaration_id: manifest.declaration_id.clone(),
            status: manifest.status,
            program_id: program.program_id.clone(),
            root_relation_id: program.root_relation_id.clone(),
            relations,
        })
    }

    pub fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    pub fn declaration_id(&self) -> &str {
        &self.declaration_id
    }

    pub fn status(&self) -> ScopeProgramStatus {
        self.status
    }

    pub fn program_id(&self) -> &str {
        &self.program_id
    }

    pub fn relation(&self, relation_id: &str) -> Option<&ScopeRelationDeclaration> {
        self.relations
            .get(relation_id)
            .map(|relation| &relation.declaration)
    }

    pub fn reserve(
        &self,
        request: ScopeAccessRequest<'_>,
    ) -> Result<ScopeAccessReservation, AccessBudgetError> {
        let relation = self
            .relations
            .get(request.relation_id)
            .ok_or_else(|| self.denied(request.relation_id, ScopeAccessDenial::UnknownRelation))?;
        if !primitive_allows_operation(relation.declaration.primitive, request.operation) {
            return Err(self.denied(request.relation_id, ScopeAccessDenial::OperationNotAllowed));
        }
        if request.identity_inputs.len() != relation.declaration.identity_inputs.len()
            || request
                .identity_inputs
                .iter()
                .zip(&relation.declaration.identity_inputs)
                .any(|(actual, expected)| {
                    actual.name != expected
                        || actual.value.is_empty()
                        || actual.value.len() > MAX_IDENTITY_VALUE_BYTES
                })
        {
            return Err(self.denied(
                request.relation_id,
                ScopeAccessDenial::IdentityInputsMismatch,
            ));
        }
        let invalid_reservation = request.max_bytes == 0
            || match request.operation {
                AccessOperation::ParameterizedQuery => request.max_rows == 0,
                AccessOperation::ObjectRead | AccessOperation::ObjectListing => {
                    request.max_rows != 0
                }
            };
        if invalid_reservation {
            return Err(self.denied(request.relation_id, ScopeAccessDenial::InvalidReservation));
        }

        let mut token_components = Vec::with_capacity(request.identity_inputs.len() * 2);
        for input in request.identity_inputs {
            token_components.push(input.name.as_bytes());
            token_components.push(input.value);
        }
        let object_token = AccessObjectToken::derive(request.relation_id, &token_components)?;
        let reservation = relation.budget.reserve(AccessReservationRequest {
            operation: request.operation,
            phase: request.phase,
            parent_token: request.parent_token,
            object_token,
            depth: request.depth,
            max_bytes: request.max_bytes,
            max_rows: request.max_rows,
        })?;
        Ok(ScopeAccessReservation {
            declaration: relation.declaration.clone(),
            object_token,
            reservation: Some(reservation),
        })
    }

    fn snapshot(&self, relation_id: &str) -> Option<AccessBudgetSnapshot> {
        self.relations
            .get(relation_id)
            .map(|relation| relation.budget.snapshot())
    }

    fn snapshots(&self) -> Vec<AccessBudgetSnapshot> {
        self.relations
            .values()
            .map(|relation| relation.budget.snapshot())
            .collect()
    }

    fn denied(&self, relation_id: &str, reason: ScopeAccessDenial) -> AccessBudgetError {
        AccessBudgetError::ScopeDenied {
            program_id: self.program_id.clone(),
            relation_id: relation_id.to_string(),
            reason,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeAccessReportDigest([u8; 32]);

impl ScopeAccessReportDigest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for ScopeAccessReportDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeAccessReport {
    scope_access_report_contract_version: u32,
    adapter_id: String,
    support_release_id: String,
    support_release_digest: [u8; 32],
    scope_program_digest: [u8; 32],
    declaration_id: String,
    program_id: String,
    selection_contract_version: u32,
    observation_contract_version: u32,
    relations: Vec<AccessBudgetSnapshot>,
    digest: ScopeAccessReportDigest,
}

impl ScopeAccessReport {
    fn new(plan: &AuthorizedScopeAccessPlan) -> Self {
        let mut report = Self {
            scope_access_report_contract_version: SCOPE_ACCESS_REPORT_CONTRACT_VERSION,
            adapter_id: plan.adapter_id().to_string(),
            support_release_id: plan.support_release_id.clone(),
            support_release_digest: plan.support_release_digest,
            scope_program_digest: plan.scope_program_digest,
            declaration_id: plan.declaration_id().to_string(),
            program_id: plan.program_id().to_string(),
            selection_contract_version: plan.selection_contract_version,
            observation_contract_version: plan.observation_contract_version,
            relations: plan.inner.snapshots(),
            digest: ScopeAccessReportDigest([0; 32]),
        };
        report.digest = report.calculate_digest();
        report
    }

    pub fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    pub fn contract_version(&self) -> u32 {
        self.scope_access_report_contract_version
    }

    pub fn support_release_id(&self) -> &str {
        &self.support_release_id
    }

    pub fn support_release_digest(&self) -> &[u8; 32] {
        &self.support_release_digest
    }

    pub fn scope_program_digest(&self) -> &[u8; 32] {
        &self.scope_program_digest
    }

    pub fn declaration_id(&self) -> &str {
        &self.declaration_id
    }

    pub fn program_id(&self) -> &str {
        &self.program_id
    }

    pub fn selection_contract_version(&self) -> u32 {
        self.selection_contract_version
    }

    pub fn observation_contract_version(&self) -> u32 {
        self.observation_contract_version
    }

    pub fn relations(&self) -> &[AccessBudgetSnapshot] {
        &self.relations
    }

    pub fn digest(&self) -> ScopeAccessReportDigest {
        self.digest
    }

    pub fn verify_digest(&self) -> bool {
        self.has_canonical_structure() && self.digest == self.calculate_digest()
    }

    fn has_canonical_structure(&self) -> bool {
        if self.scope_access_report_contract_version != SCOPE_ACCESS_REPORT_CONTRACT_VERSION
            || self.adapter_id.is_empty()
            || self.support_release_id.is_empty()
            || self.declaration_id.is_empty()
            || self.program_id.is_empty()
            || self.selection_contract_version == 0
            || self.observation_contract_version == 0
            || self.relations.is_empty()
        {
            return false;
        }
        let mut previous_relation: Option<&str> = None;
        for relation in &self.relations {
            if relation.access_trace_contract_version != ACCESS_TRACE_CONTRACT_VERSION
                || relation.bounds.validate().is_err()
                || relation.trace.len() > MAX_TRACE_CAPACITY
                || previous_relation
                    .is_some_and(|previous| previous >= relation.relation_id.as_str())
            {
                return false;
            }
            previous_relation = Some(&relation.relation_id);
            let mut previous_sequence = 0;
            for entry in &relation.trace {
                if entry.access_trace_contract_version != ACCESS_TRACE_CONTRACT_VERSION
                    || entry.relation_id != relation.relation_id
                    || entry.sequence <= previous_sequence
                {
                    return false;
                }
                previous_sequence = entry.sequence;
            }
        }
        true
    }

    fn calculate_digest(&self) -> ScopeAccessReportDigest {
        let mut hasher = Sha256::new();
        hasher.update(b"spaghetti/rfc012a/scope-access-report/v1\0");
        hash_u32(&mut hasher, self.scope_access_report_contract_version);
        report_hash_component(&mut hasher, self.adapter_id.as_bytes());
        report_hash_component(&mut hasher, self.support_release_id.as_bytes());
        report_hash_component(&mut hasher, &self.support_release_digest);
        report_hash_component(&mut hasher, &self.scope_program_digest);
        report_hash_component(&mut hasher, self.declaration_id.as_bytes());
        report_hash_component(&mut hasher, self.program_id.as_bytes());
        hash_u32(&mut hasher, self.selection_contract_version);
        hash_u32(&mut hasher, self.observation_contract_version);
        hash_u64(&mut hasher, self.relations.len() as u64);
        for relation in &self.relations {
            hash_access_budget_snapshot(&mut hasher, relation);
        }
        ScopeAccessReportDigest(hasher.finalize().into())
    }
}

fn hash_access_budget_snapshot(hasher: &mut Sha256, snapshot: &AccessBudgetSnapshot) {
    hash_u32(hasher, snapshot.access_trace_contract_version);
    report_hash_component(hasher, snapshot.relation_id.as_bytes());
    hash_u64(hasher, snapshot.bounds.max_fan_out);
    hash_u32(hasher, snapshot.bounds.max_depth);
    hash_u64(hasher, snapshot.bounds.max_objects);
    hash_u64(hasher, snapshot.bounds.max_bytes);
    hash_u64(hasher, snapshot.bounds.max_rows);
    for value in [
        snapshot.attempts,
        snapshot.reservations_granted,
        snapshot.completed,
        snapshot.denied,
        snapshot.abandoned,
        snapshot.objects_accessed,
        snapshot.bytes_read,
        snapshot.rows_read,
    ] {
        hash_u64(hasher, value);
    }
    hash_u32(hasher, snapshot.max_depth_observed);
    for value in [
        snapshot.bytes_reserved,
        snapshot.rows_reserved,
        snapshot.trace_entries_dropped,
        snapshot.trace.len() as u64,
    ] {
        hash_u64(hasher, value);
    }
    for entry in &snapshot.trace {
        hash_access_trace_entry(hasher, entry);
    }
}

fn hash_access_trace_entry(hasher: &mut Sha256, entry: &AccessTraceEntry) {
    hash_u32(hasher, entry.access_trace_contract_version);
    hash_u64(hasher, entry.sequence);
    report_hash_component(hasher, entry.relation_id.as_bytes());
    hash_u8(hasher, access_operation_code(entry.operation));
    hash_u8(hasher, access_phase_code(entry.phase));
    match entry.parent_token {
        Some(token) => {
            hash_u8(hasher, 1);
            report_hash_component(hasher, token.as_bytes());
        }
        None => hash_u8(hasher, 0),
    }
    report_hash_component(hasher, entry.object_token.as_bytes());
    hash_u32(hasher, entry.depth);
    for value in [
        entry.reserved_bytes,
        entry.reserved_rows,
        entry.bytes_read,
        entry.rows_read,
    ] {
        hash_u64(hasher, value);
    }
    hash_u8(hasher, access_outcome_code(entry.outcome));
    match entry.denied_limit {
        Some(limit) => {
            hash_u8(hasher, 1);
            hash_u8(hasher, access_limit_code(limit));
        }
        None => hash_u8(hasher, 0),
    }
}

fn report_hash_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_u8(hasher: &mut Sha256, value: u8) {
    hasher.update([value]);
}

fn hash_u32(hasher: &mut Sha256, value: u32) {
    hasher.update(value.to_be_bytes());
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

fn access_operation_code(value: AccessOperation) -> u8 {
    match value {
        AccessOperation::ObjectRead => 1,
        AccessOperation::ParameterizedQuery => 2,
        AccessOperation::ObjectListing => 3,
    }
}

fn access_phase_code(value: AccessPhase) -> u8 {
    match value {
        AccessPhase::Initial => 1,
        AccessPhase::Revalidation => 2,
    }
}

fn access_outcome_code(value: AccessOutcome) -> u8 {
    match value {
        AccessOutcome::Available => 1,
        AccessOutcome::Unavailable => 2,
        AccessOutcome::Oversized => 3,
        AccessOutcome::Failed => 4,
        AccessOutcome::Abandoned => 5,
        AccessOutcome::Denied => 6,
    }
}

fn access_limit_code(value: AccessLimit) -> u8 {
    match value {
        AccessLimit::MaxFanOut => 1,
        AccessLimit::MaxDepth => 2,
        AccessLimit::MaxObjects => 3,
        AccessLimit::MaxBytes => 4,
        AccessLimit::MaxRows => 5,
        AccessLimit::Reservation => 6,
    }
}

/// A granted reservation plus the immutable relation data the common driver
/// must use for the native access. Dropping it consumes the reservation
/// conservatively through [`AccessReservation`].
pub struct ScopeAccessReservation {
    declaration: ScopeRelationDeclaration,
    object_token: AccessObjectToken,
    reservation: Option<AccessReservation>,
}

/// One non-serializable source reservation minted only from a typed scoped
/// support authorization. The values identify reviewed declaration
/// coordinates, not native paths, and do not themselves open a source.
pub(crate) struct AuthorizedObservationSourceReservation {
    reservation: ScopeAccessReservation,
    adapter_id: String,
    program_id: String,
    support_release_id: String,
    binding: ScopeObservationSourceBinding,
    source_contract: AuthorizedObservationSourceContract,
    locator: PathBuf,
    support_release_digest: [u8; 32],
    source_declaration_digest: [u8; 32],
    scope_program_digest: [u8; 32],
}

/// One declaration reservation bound to the exact runtime stream returned by
/// the selected adapter for one source instance. This still carries no native
/// root authority: the future attachment owner must match that instance's root
/// to its separately approved access-root grant before invoking a driver.
pub(crate) struct AuthorizedObservationRuntimeStreamReservation {
    reservation: AuthorizedObservationSourceReservation,
    source_instance_id: u64,
    source_instance_identity_contract_version: u32,
    source_instance_key: SourceInstanceKey,
    stream: StreamSpec,
}

/// One opaque directory-entry reservation minted beneath an already-authorized
/// listing root. It retains no native name or path and must complete before a
/// discovered entry can become membership evidence.
pub(crate) struct AuthorizedObservationDirectoryEntryReservation {
    object_token: AccessObjectToken,
    reservation: AccessReservation,
    listing_state: Arc<Mutex<AuthorizedObservationDirectoryListingState>>,
    coordinate: AuthorizedObservationDirectoryMemberCoordinate,
}

/// In-memory identity of one exact listing-root reservation. Pointer identity
/// and sequence prevent a proof prepared from one pass from spending another
/// pass's otherwise equal relation coordinates.
#[derive(Clone)]
pub(crate) struct AuthorizedObservationDirectoryRootAuthority {
    inner: Arc<AccessBudgetInner>,
    sequence: u64,
    object_token: AccessObjectToken,
    listing_state: Arc<Mutex<AuthorizedObservationDirectoryListingState>>,
}

/// Post-listing authority for exactly the opaque children accounted by one
/// completed directory pass. It carries the reviewed ReplaceDocument bound,
/// but no native path or caller-constructible identity.
pub(crate) struct AuthorizedObservationDirectoryReadAuthority {
    inner: Arc<AccessBudgetInner>,
    root_object_token: AccessObjectToken,
    phase: AccessPhase,
    max_object_bytes: u64,
    listing_state: Arc<Mutex<AuthorizedObservationDirectoryListingState>>,
}

/// One-shot read reservation for an exact child already admitted by the
/// listing authority. Dropping it consumes the declared byte bound.
pub(crate) struct AuthorizedObservationDirectoryMemberReadReservation {
    reservation: AccessReservation,
    max_object_bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AuthorizedObservationDirectoryMemberCoordinate {
    parent_token: AccessObjectToken,
    object_token: AccessObjectToken,
    depth: u32,
}

#[derive(Default)]
struct AuthorizedObservationDirectoryListingState {
    sealed: bool,
    accounted: BTreeMap<AuthorizedObservationDirectoryMemberCoordinate, bool>,
    reads_reserved: BTreeSet<AuthorizedObservationDirectoryMemberCoordinate>,
}

impl std::fmt::Debug for AuthorizedObservationSourceReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedObservationSourceReservation")
            .field("primitive", &self.reservation.primitive())
            .field(
                "has_relative_selector",
                &self.binding.relative_selector.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AuthorizedObservationRuntimeStreamReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedObservationRuntimeStreamReservation")
            .field("primitive", &self.reservation.primitive())
            .field(
                "has_relative_selector",
                &self.reservation.relative_selector().is_some(),
            )
            .field("has_source_instance", &true)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AuthorizedObservationDirectoryEntryReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedObservationDirectoryEntryReservation")
            .field("has_object_token", &true)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AuthorizedObservationDirectoryRootAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedObservationDirectoryRootAuthority")
            .field("has_access_pass", &true)
            .field("has_object_token", &true)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AuthorizedObservationDirectoryReadAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedObservationDirectoryReadAuthority")
            .field("has_access_pass", &true)
            .field("has_root_object_token", &true)
            .field("max_object_bytes", &self.max_object_bytes)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AuthorizedObservationDirectoryMemberReadReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedObservationDirectoryMemberReadReservation")
            .field("max_object_bytes", &self.max_object_bytes)
            .finish_non_exhaustive()
    }
}

impl AuthorizedObservationSourceReservation {
    pub(crate) fn relation_id(&self) -> &str {
        self.reservation.relation_id()
    }

    pub(crate) fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    pub(crate) fn program_id(&self) -> &str {
        &self.program_id
    }

    pub(crate) fn primitive(&self) -> ScopeRelationPrimitive {
        self.reservation.primitive()
    }

    pub(crate) fn operation(&self) -> AccessOperation {
        self.reservation.operation()
    }

    pub(crate) fn bounds(&self) -> ScopeRelationBounds {
        self.reservation.bounds()
    }

    pub(crate) fn access_root(&self) -> &str {
        self.reservation.access_root()
    }

    pub(crate) fn stream_id(&self) -> &str {
        self.source_contract.stream_id()
    }

    pub(crate) fn source_pattern(&self) -> &str {
        &self.binding.source_pattern
    }

    pub(crate) fn relative_selector(&self) -> Option<&str> {
        self.binding.relative_selector.as_deref()
    }

    pub(crate) fn driver(&self) -> AuthorizedObservationSourceDriver {
        self.source_contract.driver()
    }

    pub(crate) fn locator(&self) -> &Path {
        &self.locator
    }

    pub(crate) fn support_release_digest(&self) -> &[u8; 32] {
        &self.support_release_digest
    }

    pub(crate) fn source_declaration_digest(&self) -> &[u8; 32] {
        &self.source_declaration_digest
    }

    pub(crate) fn scope_program_digest(&self) -> &[u8; 32] {
        &self.scope_program_digest
    }

    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.reservation.object_token()
    }

    fn reserve_directory_entry(
        &self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        relative_path_key: &[u8],
        parent_relative_path_key: Option<&[u8]>,
        depth: u32,
    ) -> Result<AuthorizedObservationDirectoryEntryReservation, AccessBudgetError> {
        self.reservation.reserve_directory_entry(
            authority,
            relative_path_key,
            parent_relative_path_key,
            depth,
        )
    }

    fn directory_entry_token(
        &self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        relative_path_key: &[u8],
    ) -> Result<AccessObjectToken, AccessBudgetError> {
        self.reservation
            .directory_entry_token(authority, relative_path_key)
    }

    fn directory_root_authority(
        &self,
    ) -> Result<AuthorizedObservationDirectoryRootAuthority, AccessBudgetError> {
        self.reservation.directory_root_authority()
    }

    /// Select exactly one stream from the adapter package already bound by the
    /// typed support authorization. Any manifest, source-instance, selector,
    /// decoder, authority, lifecycle, or driver-bound drift consumes the
    /// reservation conservatively and reports one path-free failure.
    pub(crate) fn bind_runtime_stream(
        self,
        adapter: &dyn AgentAdapter,
        instance: &SourceInstance,
    ) -> Result<AuthorizedObservationRuntimeStreamReservation, AccessBudgetError> {
        let stream = match self.select_runtime_stream(adapter, instance) {
            Ok(stream) => stream,
            Err(error) => {
                self.fail_conservative();
                return Err(error);
            }
        };
        Ok(AuthorizedObservationRuntimeStreamReservation {
            reservation: self,
            source_instance_id: instance.id,
            source_instance_identity_contract_version: instance.spec.identity_contract_version,
            source_instance_key: instance.spec.stable_key.clone(),
            stream,
        })
    }

    fn select_runtime_stream(
        &self,
        adapter: &dyn AgentAdapter,
        instance: &SourceInstance,
    ) -> Result<StreamSpec, AccessBudgetError> {
        let manifest = adapter.manifest();
        let support_binding = manifest
            .support_binding
            .as_ref()
            .ok_or_else(invalid_observation_runtime_stream_binding)?;
        if manifest.validate().is_err()
            || manifest.id.as_str() != self.adapter_id
            || support_binding.support_release_id() != self.support_release_id
            || support_binding.source_declaration_digest().as_bytes()
                != &self.source_declaration_digest
            || support_binding.scope_program_digest().as_bytes() != &self.scope_program_digest
            || instance.id == 0
            || instance.spec.validate().is_err()
            || instance
                .spec
                .roots
                .iter()
                .filter(|root| root.name == self.access_root())
                .count()
                != 1
        {
            return Err(invalid_observation_runtime_stream_binding());
        }

        let streams = adapter
            .streams(instance)
            .map_err(|_| invalid_observation_runtime_stream_binding())?;
        let mut selected = None;
        for stream in streams {
            if stream.id.as_str() != self.stream_id() {
                continue;
            }
            if selected.replace(stream).is_some() {
                return Err(invalid_observation_runtime_stream_binding());
            }
        }
        let stream = selected.ok_or_else(invalid_observation_runtime_stream_binding)?;
        if stream.validate(instance).is_err() || !self.runtime_stream_matches(&stream) {
            return Err(invalid_observation_runtime_stream_binding());
        }
        Ok(stream)
    }

    fn runtime_stream_matches(&self, stream: &StreamSpec) -> bool {
        let authority = match self.source_contract.authority() {
            AuthorizedObservationSourceAuthority::Canonical => StreamAuthority::Canonical,
            AuthorizedObservationSourceAuthority::Supplemental => StreamAuthority::Supplemental,
            AuthorizedObservationSourceAuthority::Diagnostic => StreamAuthority::Diagnostic,
            AuthorizedObservationSourceAuthority::IgnoredDerived => StreamAuthority::IgnoredDerived,
        };
        let driver_matches = match (&stream.driver, self.source_contract.driver()) {
            (
                DriverSpec::AppendDelimited(runtime),
                AuthorizedObservationSourceDriver::AppendDelimited {
                    max_record_bytes,
                    max_batch_bytes,
                    max_records_per_batch,
                },
            ) => {
                u64::try_from(runtime.max_record_bytes).ok() == Some(max_record_bytes)
                    && u64::try_from(runtime.max_batch_bytes).ok() == Some(max_batch_bytes)
                    && u64::try_from(runtime.max_records_per_batch).ok()
                        == Some(max_records_per_batch)
                    && stream.consistency == ConsistencyPolicy::IncrementalCursor
            }
            (
                DriverSpec::ReplaceDocument(runtime),
                AuthorizedObservationSourceDriver::ReplaceDocument { max_object_bytes },
            ) => {
                u64::try_from(runtime.max_document_bytes).ok() == Some(max_object_bytes)
                    && stream.consistency == ConsistencyPolicy::SnapshotReplace
            }
            (
                DriverSpec::Presence(runtime),
                AuthorizedObservationSourceDriver::PresenceObject { max_object_bytes },
            ) => {
                runtime.include_content
                    && u64::try_from(runtime.max_content_bytes).ok() == Some(max_object_bytes)
                    && stream.consistency == ConsistencyPolicy::SnapshotReplace
            }
            _ => false,
        };
        stream.selector.root_name == self.access_root()
            && stream.selector.include.as_slice() == self.source_contract.relative_patterns()
            && stream.selector.exclude.is_empty()
            && stream.decoder.as_str() == self.source_contract.decoder_id()
            && stream.authority == authority
            && stream.deletion == DeletionPolicy::MirrorSource
            && self
                .source_contract
                .relative_patterns()
                .iter()
                .filter(|pattern| pattern.as_str() == self.source_pattern())
                .count()
                == 1
            && driver_matches
    }

    pub(crate) fn complete(
        self,
        bytes_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.reservation.complete(bytes_read, 0, outcome)
    }

    pub(crate) fn fail_conservative(self) {
        self.reservation.fail_conservative();
    }
}

impl AuthorizedObservationRuntimeStreamReservation {
    pub(crate) fn relation_id(&self) -> &str {
        self.reservation.relation_id()
    }

    pub(crate) fn adapter_id(&self) -> &str {
        self.reservation.adapter_id()
    }

    pub(crate) fn program_id(&self) -> &str {
        self.reservation.program_id()
    }

    pub(crate) fn primitive(&self) -> ScopeRelationPrimitive {
        self.reservation.primitive()
    }

    pub(crate) fn operation(&self) -> AccessOperation {
        self.reservation.operation()
    }

    pub(crate) fn bounds(&self) -> ScopeRelationBounds {
        self.reservation.bounds()
    }

    pub(crate) fn access_root(&self) -> &str {
        self.reservation.access_root()
    }

    pub(crate) fn locator(&self) -> &Path {
        self.reservation.locator()
    }

    pub(crate) fn relative_selector(&self) -> Option<&str> {
        self.reservation.relative_selector()
    }

    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.reservation.object_token()
    }

    pub(crate) fn source_instance_id(&self) -> u64 {
        self.source_instance_id
    }

    pub(crate) fn source_instance_identity_contract_version(&self) -> u32 {
        self.source_instance_identity_contract_version
    }

    pub(crate) fn source_instance_key(&self) -> &SourceInstanceKey {
        &self.source_instance_key
    }

    pub(crate) fn stream(&self) -> &StreamSpec {
        &self.stream
    }

    pub(crate) fn support_release_digest(&self) -> &[u8; 32] {
        self.reservation.support_release_digest()
    }

    pub(crate) fn source_declaration_digest(&self) -> &[u8; 32] {
        self.reservation.source_declaration_digest()
    }

    pub(crate) fn scope_program_digest(&self) -> &[u8; 32] {
        self.reservation.scope_program_digest()
    }

    /// Reserve one entry yielded beneath this exact directory-listing root.
    /// Relative path keys are common-driver binary identities and are reduced
    /// immediately to opaque tokens; neither the reservation nor its trace
    /// retains native path material.
    pub(crate) fn reserve_directory_entry(
        &self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        relative_path_key: &[u8],
        parent_relative_path_key: Option<&[u8]>,
        depth: u32,
    ) -> Result<AuthorizedObservationDirectoryEntryReservation, AccessBudgetError> {
        self.reservation.reserve_directory_entry(
            authority,
            relative_path_key,
            parent_relative_path_key,
            depth,
        )
    }

    pub(crate) fn directory_entry_token(
        &self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        relative_path_key: &[u8],
    ) -> Result<AccessObjectToken, AccessBudgetError> {
        self.reservation
            .directory_entry_token(authority, relative_path_key)
    }

    pub(crate) fn directory_root_authority(
        &self,
    ) -> Result<AuthorizedObservationDirectoryRootAuthority, AccessBudgetError> {
        self.reservation.directory_root_authority()
    }

    /// Seal this exact audited listing and, only for an available
    /// ChildDirectory/ReplaceDocument stream, mint authority to read its
    /// accounted children under the declaration-owned object bound.
    pub(crate) fn complete_directory_listing(
        self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        outcome: AccessOutcome,
    ) -> Result<Option<AuthorizedObservationDirectoryReadAuthority>, AccessBudgetError> {
        let max_object_bytes = match self.reservation.driver() {
            AuthorizedObservationSourceDriver::ReplaceDocument { max_object_bytes }
                if max_object_bytes > 0 =>
            {
                max_object_bytes
            }
            _ => {
                self.fail_conservative();
                return Err(invalid_directory_read_authority());
            }
        };
        self.reservation.reservation.complete_directory_listing(
            authority,
            max_object_bytes,
            outcome,
        )
    }

    pub(crate) fn complete(
        self,
        bytes_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.reservation.complete(bytes_read, outcome)
    }

    pub(crate) fn fail_conservative(self) {
        self.reservation.fail_conservative();
    }
}

impl AuthorizedObservationDirectoryEntryReservation {
    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.object_token
    }

    pub(crate) fn complete(self, selected_file: bool) -> Result<(), AccessBudgetError> {
        let Self {
            reservation,
            listing_state,
            coordinate,
            ..
        } = self;
        let mut state = listing_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.sealed || state.accounted.contains_key(&coordinate) {
            reservation.fail_conservative();
            return Err(invalid_directory_entry_reservation());
        }
        reservation.complete(0, 0, AccessOutcome::Available)?;
        state.accounted.insert(coordinate, selected_file);
        Ok(())
    }

    pub(crate) fn fail_conservative(self) {
        self.reservation.fail_conservative();
    }
}

impl AuthorizedObservationDirectoryReadAuthority {
    pub(crate) fn max_object_bytes(&self) -> u64 {
        self.max_object_bytes
    }

    pub(crate) fn reserve_member_read(
        &self,
        object_token: AccessObjectToken,
        parent_token: AccessObjectToken,
        depth: u32,
    ) -> Result<AuthorizedObservationDirectoryMemberReadReservation, AccessBudgetError> {
        let coordinate = AuthorizedObservationDirectoryMemberCoordinate {
            parent_token,
            object_token,
            depth,
        };
        let mut state = self
            .listing_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.sealed
            || object_token == self.root_object_token
            || parent_token == object_token
            || state.accounted.get(&coordinate) != Some(&true)
            || !state.reads_reserved.insert(coordinate)
        {
            return Err(invalid_directory_member_read_reservation());
        }
        let reservation = AccessBudget {
            inner: Arc::clone(&self.inner),
        }
        .reserve(AccessReservationRequest {
            operation: AccessOperation::ObjectRead,
            phase: self.phase,
            parent_token: Some(parent_token),
            object_token,
            depth,
            max_bytes: self.max_object_bytes,
            max_rows: 0,
        })?;
        Ok(AuthorizedObservationDirectoryMemberReadReservation {
            reservation,
            max_object_bytes: self.max_object_bytes,
        })
    }
}

impl AuthorizedObservationDirectoryMemberReadReservation {
    pub(crate) fn max_object_bytes(&self) -> u64 {
        self.max_object_bytes
    }

    pub(crate) fn complete(
        self,
        bytes_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.reservation.complete(bytes_read, 0, outcome)
    }

    pub(crate) fn fail_conservative(self) {
        self.reservation.fail_conservative();
    }
}

impl ScopeAccessReservation {
    pub fn relation_id(&self) -> &str {
        &self.declaration.relation_id
    }

    pub fn primitive(&self) -> ScopeRelationPrimitive {
        self.declaration.primitive
    }

    pub(crate) fn operation(&self) -> AccessOperation {
        self.reservation
            .as_ref()
            .expect("scope reservation is consumed only once")
            .request
            .operation
    }

    pub(crate) fn bounds(&self) -> ScopeRelationBounds {
        self.declaration.bounds
    }

    pub fn access_root(&self) -> &str {
        &self.declaration.access_root
    }

    pub fn locator(&self) -> &str {
        &self.declaration.locator
    }

    pub fn statement_id(&self) -> Option<&str> {
        self.declaration.statement_id.as_deref()
    }

    pub fn parameter_names(&self) -> Option<&[String]> {
        self.declaration.parameter_names.as_deref()
    }

    pub fn unavailable_behavior(&self) -> ScopeUnavailableBehavior {
        self.declaration.unavailable_behavior
    }

    pub fn object_token(&self) -> AccessObjectToken {
        self.object_token
    }

    fn reserve_directory_entry(
        &self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        relative_path_key: &[u8],
        parent_relative_path_key: Option<&[u8]>,
        depth: u32,
    ) -> Result<AuthorizedObservationDirectoryEntryReservation, AccessBudgetError> {
        if self.declaration.primitive != ScopeRelationPrimitive::ChildDirectoryByNativeId
            || self.operation() != AccessOperation::ObjectListing
            || !self.matches_directory_root_authority(authority)
            || relative_path_key.len() <= 1
            || parent_relative_path_key
                .is_some_and(|parent| parent.len() <= 1 || parent == relative_path_key)
        {
            return Err(invalid_directory_entry_reservation());
        }
        let object_token = self.directory_entry_token(authority, relative_path_key)?;
        let parent_token = match parent_relative_path_key {
            Some(parent) => self.directory_entry_token(authority, parent)?,
            None => self.object_token,
        };
        let reservation = self
            .reservation
            .as_ref()
            .expect("scope reservation is consumed only once")
            .reserve_related(AccessReservationRequest {
                operation: AccessOperation::ObjectListing,
                phase: self.operation_phase(),
                parent_token: Some(parent_token),
                object_token,
                depth,
                max_bytes: 0,
                max_rows: 0,
            })?;
        Ok(AuthorizedObservationDirectoryEntryReservation {
            object_token,
            reservation,
            listing_state: Arc::clone(&authority.listing_state),
            coordinate: AuthorizedObservationDirectoryMemberCoordinate {
                parent_token,
                object_token,
                depth,
            },
        })
    }

    fn directory_entry_token(
        &self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        relative_path_key: &[u8],
    ) -> Result<AccessObjectToken, AccessBudgetError> {
        if relative_path_key.len() <= 1 || !self.matches_directory_root_authority(authority) {
            return Err(invalid_directory_entry_reservation());
        }
        AccessObjectToken::derive(
            self.relation_id(),
            &[
                DIRECTORY_ENTRY_TOKEN_DOMAIN,
                self.object_token.as_bytes(),
                relative_path_key,
            ],
        )
    }

    fn directory_root_authority(
        &self,
    ) -> Result<AuthorizedObservationDirectoryRootAuthority, AccessBudgetError> {
        if self.declaration.primitive != ScopeRelationPrimitive::ChildDirectoryByNativeId
            || self.operation() != AccessOperation::ObjectListing
        {
            return Err(invalid_directory_entry_reservation());
        }
        let reservation = self
            .reservation
            .as_ref()
            .expect("scope reservation is consumed only once");
        Ok(AuthorizedObservationDirectoryRootAuthority {
            inner: Arc::clone(&reservation.inner),
            sequence: reservation.sequence,
            object_token: self.object_token,
            listing_state: Arc::new(Mutex::new(
                AuthorizedObservationDirectoryListingState::default(),
            )),
        })
    }

    fn complete_directory_listing(
        self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        max_object_bytes: u64,
        outcome: AccessOutcome,
    ) -> Result<Option<AuthorizedObservationDirectoryReadAuthority>, AccessBudgetError> {
        if self.declaration.primitive != ScopeRelationPrimitive::ChildDirectoryByNativeId
            || self.operation() != AccessOperation::ObjectListing
            || max_object_bytes == 0
            || !matches!(
                outcome,
                AccessOutcome::Available | AccessOutcome::Unavailable
            )
            || !self.matches_directory_root_authority(authority)
        {
            self.fail_conservative();
            return Err(invalid_directory_read_authority());
        }
        let reservation = self
            .reservation
            .as_ref()
            .expect("scope reservation is consumed only once");
        let inner = Arc::clone(&reservation.inner);
        let phase = reservation.request.phase;
        {
            let mut state = authority
                .listing_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.sealed
                || (outcome == AccessOutcome::Unavailable && !state.accounted.is_empty())
            {
                drop(state);
                self.fail_conservative();
                return Err(invalid_directory_read_authority());
            }
            state.sealed = true;
        }
        let read_authority = (outcome == AccessOutcome::Available).then(|| {
            AuthorizedObservationDirectoryReadAuthority {
                inner,
                root_object_token: self.object_token,
                phase,
                max_object_bytes,
                listing_state: Arc::clone(&authority.listing_state),
            }
        });
        self.complete(0, 0, outcome)?;
        Ok(read_authority)
    }

    fn matches_directory_root_authority(
        &self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
    ) -> bool {
        let reservation = self
            .reservation
            .as_ref()
            .expect("scope reservation is consumed only once");
        Arc::ptr_eq(&authority.inner, &reservation.inner)
            && authority.sequence == reservation.sequence
            && authority.object_token == self.object_token
    }

    fn operation_phase(&self) -> AccessPhase {
        self.reservation
            .as_ref()
            .expect("scope reservation is consumed only once")
            .request
            .phase
    }

    /// Render the declaration's exact evidence locator using the same named
    /// values that minted this reservation's opaque object token. The result
    /// is only a confined relative path; this method neither joins it to a
    /// native root nor opens an object.
    pub(crate) fn render_evidence_locator(
        &self,
        identity_inputs: &[ScopeIdentityInput<'_>],
    ) -> Result<PathBuf, AccessBudgetError> {
        validate_evidence_locator_template(&self.declaration)?;
        self.validate_locator_identity_binding(identity_inputs)?;
        render_confined_locator(&self.declaration.locator, identity_inputs)
    }

    /// Render one `ChildDirectoryByNativeId` membership root after the exact
    /// identity inputs have already minted this reservation's opaque object
    /// token. The result remains a confined relative path; source-root
    /// authority and native access stay with the scoped host.
    pub(crate) fn render_child_directory_locator(
        &self,
        identity_inputs: &[ScopeIdentityInput<'_>],
    ) -> Result<PathBuf, AccessBudgetError> {
        validate_child_directory_locator_template(&self.declaration)?;
        self.validate_locator_identity_binding(identity_inputs)?;
        render_confined_locator(&self.declaration.locator, identity_inputs)
    }

    /// Render one fixed sibling or evidence-referenced observation object
    /// after the exact declared identity inputs have minted this reservation's
    /// opaque token. The result remains relative to the selected host root and
    /// does not itself authorize or perform a native read.
    pub(crate) fn render_related_object_locator(
        &self,
        identity_inputs: &[ScopeIdentityInput<'_>],
    ) -> Result<PathBuf, AccessBudgetError> {
        validate_related_object_locator_template(&self.declaration)?;
        self.validate_locator_identity_binding(identity_inputs)?;
        render_confined_locator(&self.declaration.locator, identity_inputs)
    }

    fn validate_locator_identity_binding(
        &self,
        identity_inputs: &[ScopeIdentityInput<'_>],
    ) -> Result<(), AccessBudgetError> {
        if identity_inputs.len() != self.declaration.identity_inputs.len()
            || identity_inputs
                .iter()
                .zip(&self.declaration.identity_inputs)
                .any(|(actual, expected)| actual.name != expected)
        {
            return Err(invalid_locator_template());
        }
        let mut token_components = Vec::with_capacity(identity_inputs.len() * 2);
        for input in identity_inputs {
            token_components.push(input.name.as_bytes());
            token_components.push(input.value);
        }
        if AccessObjectToken::derive(self.relation_id(), &token_components)? != self.object_token {
            return Err(invalid_locator_template());
        }
        Ok(())
    }

    pub fn complete(
        mut self,
        bytes_read: u64,
        rows_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.reservation
            .take()
            .expect("scope reservation is consumed only once")
            .complete(bytes_read, rows_read, outcome)
    }

    pub fn fail_conservative(mut self) {
        self.reservation
            .take()
            .expect("scope reservation is consumed only once")
            .fail_conservative();
    }
}

pub(crate) fn validate_evidence_locator_template(
    declaration: &ScopeRelationDeclaration,
) -> Result<(), AccessBudgetError> {
    validate_bound_locator_template(
        declaration,
        ScopeRelationPrimitive::ArtifactLocatorFromEvidence,
    )
}

fn validate_child_directory_locator_template(
    declaration: &ScopeRelationDeclaration,
) -> Result<(), AccessBudgetError> {
    validate_bound_locator_template(
        declaration,
        ScopeRelationPrimitive::ChildDirectoryByNativeId,
    )
}

fn validate_related_object_locator_template(
    declaration: &ScopeRelationDeclaration,
) -> Result<(), AccessBudgetError> {
    if !matches!(
        declaration.primitive,
        ScopeRelationPrimitive::SiblingObject | ScopeRelationPrimitive::ReferencedObjectFromField
    ) {
        return Err(invalid_locator_template());
    }
    validate_locator_identity_placeholders(declaration)
}

fn validate_bound_locator_template(
    declaration: &ScopeRelationDeclaration,
    expected_primitive: ScopeRelationPrimitive,
) -> Result<(), AccessBudgetError> {
    if declaration.primitive != expected_primitive {
        return Err(invalid_locator_template());
    }
    validate_locator_identity_placeholders(declaration)
}

fn validate_locator_identity_placeholders(
    declaration: &ScopeRelationDeclaration,
) -> Result<(), AccessBudgetError> {
    let placeholders = locator_placeholders(&declaration.locator)?;
    if placeholders.is_empty()
        || placeholders.iter().any(|(_, _, name)| {
            !declaration
                .identity_inputs
                .iter()
                .any(|declared| declared == name)
        })
    {
        return Err(invalid_locator_template());
    }
    Ok(())
}

fn render_confined_locator(
    template: &str,
    identity_inputs: &[ScopeIdentityInput<'_>],
) -> Result<PathBuf, AccessBudgetError> {
    let placeholders = locator_placeholders(template)?;
    let values = identity_inputs
        .iter()
        .map(|input| {
            let value = std::str::from_utf8(input.value).map_err(|_| invalid_locator_template())?;
            if value.is_empty()
                || value
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\'))
            {
                return Err(invalid_locator_template());
            }
            Ok((input.name, value))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if values.len() != identity_inputs.len() {
        return Err(invalid_locator_template());
    }

    let mut output_len = template.len();
    for (start, end, name) in &placeholders {
        let value = values.get(name).ok_or_else(invalid_locator_template)?;
        output_len = output_len
            .checked_sub(end - start)
            .and_then(|length| length.checked_add(value.len()))
            .ok_or_else(invalid_locator_template)?;
    }
    if output_len == 0 || output_len > MAX_RENDERED_SCOPE_LOCATOR_BYTES {
        return Err(invalid_locator_template());
    }
    let mut output = String::new();
    output
        .try_reserve_exact(output_len)
        .map_err(|_| invalid_locator_template())?;
    let mut cursor = 0;
    for (start, end, name) in placeholders {
        output.push_str(&template[cursor..start]);
        output.push_str(values.get(name).ok_or_else(invalid_locator_template)?);
        cursor = end;
    }
    output.push_str(&template[cursor..]);
    let first_component = output.split('/').next().unwrap_or_default().as_bytes();
    let has_windows_drive_prefix = first_component.len() >= 2
        && first_component[0].is_ascii_alphabetic()
        && first_component[1] == b':';
    if output.len() != output_len
        || output.starts_with('/')
        || has_windows_drive_prefix
        || output.contains('\\')
        || output
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid_locator_template());
    }
    let path = PathBuf::from(output);
    super::file::confined_relative_path_key(&path).map_err(|_| invalid_locator_template())?;
    Ok(path)
}

fn locator_placeholders(locator: &str) -> Result<Vec<(usize, usize, &str)>, AccessBudgetError> {
    let bytes = locator.as_bytes();
    let mut placeholders = Vec::new();
    let mut names = BTreeSet::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'{' => {
                let end = bytes[cursor + 1..]
                    .iter()
                    .position(|byte| *byte == b'}')
                    .map(|offset| cursor + 1 + offset)
                    .ok_or_else(invalid_locator_template)?;
                let name = &locator[cursor + 1..end];
                if name.as_bytes().contains(&b'{')
                    || validate_relation_id(name).is_err()
                    || !names.insert(name)
                {
                    return Err(invalid_locator_template());
                }
                placeholders.push((cursor, end + 1, name));
                cursor = end + 1;
            }
            b'}' => return Err(invalid_locator_template()),
            _ => cursor += 1,
        }
    }
    Ok(placeholders)
}

fn invalid_locator_template() -> AccessBudgetError {
    AccessBudgetError::InvalidConfig(
        "scope locator template or bound identity input is invalid".to_string(),
    )
}

fn invalid_observation_source_reservation() -> AccessBudgetError {
    AccessBudgetError::InvalidConfig(
        "authorized observation source reservation requires one supported bound relation"
            .to_string(),
    )
}

fn invalid_observation_runtime_stream_binding() -> AccessBudgetError {
    AccessBudgetError::InvalidConfig(
        "authorized observation source does not match the selected adapter runtime stream"
            .to_string(),
    )
}

fn invalid_directory_entry_reservation() -> AccessBudgetError {
    AccessBudgetError::InvalidConfig(
        "authorized directory entry reservation requires one confined enumerated child".to_string(),
    )
}

fn invalid_directory_read_authority() -> AccessBudgetError {
    AccessBudgetError::InvalidConfig(
        "authorized directory read authority requires one completed bound listing".to_string(),
    )
}

fn invalid_directory_member_read_reservation() -> AccessBudgetError {
    AccessBudgetError::InvalidConfig(
        "authorized directory member read requires one unread enumerated child".to_string(),
    )
}

fn primitive_allows_operation(
    primitive: ScopeRelationPrimitive,
    operation: AccessOperation,
) -> bool {
    match primitive {
        ScopeRelationPrimitive::ParameterizedSQLiteRows => {
            operation == AccessOperation::ParameterizedQuery
        }
        ScopeRelationPrimitive::ChildDirectoryByNativeId | ScopeRelationPrimitive::KeyNamespace => {
            matches!(
                operation,
                AccessOperation::ObjectListing | AccessOperation::ObjectRead
            )
        }
        ScopeRelationPrimitive::KnownObject
        | ScopeRelationPrimitive::SiblingObject
        | ScopeRelationPrimitive::ReferencedObjectFromField
        | ScopeRelationPrimitive::BoundedIndexLookup
        | ScopeRelationPrimitive::ArtifactLocatorFromEvidence => {
            operation == AccessOperation::ObjectRead
        }
    }
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
    fn reserve_related(
        &self,
        request: AccessReservationRequest,
    ) -> Result<AccessReservation, AccessBudgetError> {
        AccessBudget {
            inner: Arc::clone(&self.inner),
        }
        .reserve(request)
    }

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

pub(crate) fn validate_relation_id(value: &str) -> Result<(), AccessBudgetError> {
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
    use crate::source::confined_relative_path_key;

    use super::*;

    #[derive(Deserialize)]
    struct SharedAccessReportFixture {
        fixture_contract_version: u32,
        report: ScopeAccessReport,
        expected_digest: String,
    }

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

    #[test]
    fn scope_access_report_matches_the_portable_digest_fixture() {
        let fixture: SharedAccessReportFixture = serde_json::from_str(include_str!(
            "../../fixtures/contracts/rfc012a-access-report-v1.json"
        ))
        .unwrap();
        assert_eq!(fixture.fixture_contract_version, 1);
        let calculated = fixture.report.calculate_digest();
        assert_eq!(calculated.to_string(), fixture.expected_digest);
        assert_eq!(calculated, fixture.report.digest());
        assert!(fixture.report.verify_digest());
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

    fn grok_scope_manifest() -> ScopeProgramManifest {
        ScopeProgramManifest::from_json(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
        )))
        .unwrap()
    }

    fn artifact_scope_plan(locator: &str) -> ScopeAccessPlan {
        let mut manifest = grok_scope_manifest();
        let relation = manifest.programs[0]
            .relations
            .iter_mut()
            .find(|relation| relation.relation_id == "summary-sidecar")
            .unwrap();
        relation.primitive = ScopeRelationPrimitive::ArtifactLocatorFromEvidence;
        relation.locator = locator.to_string();
        relation.identity_inputs = vec![
            "native-session-id".to_string(),
            "backup-name".to_string(),
            "artifact-version".to_string(),
        ];
        ScopeAccessPlan::for_program(&manifest, "observe-session-sidecars").unwrap()
    }

    fn render_artifact_locator(
        locator: &str,
        native_session_id: &[u8],
        backup_name: &[u8],
        artifact_version: &[u8],
    ) -> Result<PathBuf, AccessBudgetError> {
        let plan = artifact_scope_plan(locator);
        let identity = [
            ScopeIdentityInput {
                name: "native-session-id",
                value: native_session_id,
            },
            ScopeIdentityInput {
                name: "backup-name",
                value: backup_name,
            },
            ScopeIdentityInput {
                name: "artifact-version",
                value: artifact_version,
            },
        ];
        plan.reserve(ScopeAccessRequest {
            relation_id: "summary-sidecar",
            operation: AccessOperation::ObjectRead,
            phase: AccessPhase::Revalidation,
            parent_token: None,
            identity_inputs: &identity,
            depth: 1,
            max_bytes: 1,
            max_rows: 0,
        })?
        .render_evidence_locator(&identity)
    }

    fn child_directory_scope_plan(locator: &str) -> ScopeAccessPlan {
        let mut manifest = grok_scope_manifest();
        let relation = manifest.programs[0]
            .relations
            .iter_mut()
            .find(|relation| relation.relation_id == "summary-sidecar")
            .unwrap();
        relation.relation_id = "descendant-transcripts".to_owned();
        relation.primitive = ScopeRelationPrimitive::ChildDirectoryByNativeId;
        relation.locator = locator.to_owned();
        relation.identity_inputs = vec!["project-key".to_owned(), "native-session-id".to_owned()];
        relation.bounds.max_fan_out = 2;
        relation.bounds.max_depth = 2;
        relation.bounds.max_objects = 4;
        ScopeAccessPlan::for_program(&manifest, "observe-session-sidecars").unwrap()
    }

    fn render_child_directory_locator(
        locator: &str,
        project_key: &[u8],
        native_session_id: &[u8],
    ) -> Result<PathBuf, AccessBudgetError> {
        let plan = child_directory_scope_plan(locator);
        let identity = [
            ScopeIdentityInput {
                name: "project-key",
                value: project_key,
            },
            ScopeIdentityInput {
                name: "native-session-id",
                value: native_session_id,
            },
        ];
        plan.reserve(ScopeAccessRequest {
            relation_id: "descendant-transcripts",
            operation: AccessOperation::ObjectListing,
            phase: AccessPhase::Revalidation,
            parent_token: None,
            identity_inputs: &identity,
            depth: 1,
            max_bytes: 1,
            max_rows: 0,
        })?
        .render_child_directory_locator(&identity)
    }

    fn related_object_scope_plan(
        primitive: ScopeRelationPrimitive,
        locator: &str,
        identity_inputs: &[&str],
    ) -> ScopeAccessPlan {
        let mut manifest = grok_scope_manifest();
        let relation = manifest.programs[0]
            .relations
            .iter_mut()
            .find(|relation| relation.relation_id == "summary-sidecar")
            .unwrap();
        relation.relation_id = "related-object".to_owned();
        relation.primitive = primitive;
        relation.locator = locator.to_owned();
        relation.identity_inputs = identity_inputs
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        relation.bounds.max_fan_out = 32;
        relation.bounds.max_objects = 32;
        ScopeAccessPlan::for_program(&manifest, "observe-session-sidecars").unwrap()
    }

    fn reserve_related_object<'a>(
        plan: &'a ScopeAccessPlan,
        identity_inputs: &'a [ScopeIdentityInput<'a>],
    ) -> ScopeAccessReservation {
        plan.reserve(ScopeAccessRequest {
            relation_id: "related-object",
            operation: AccessOperation::ObjectRead,
            phase: AccessPhase::Revalidation,
            parent_token: None,
            identity_inputs,
            depth: 1,
            max_bytes: 1,
            max_rows: 0,
        })
        .unwrap()
    }

    #[test]
    fn child_directory_locator_is_confined_and_bound_to_the_reserved_identity() {
        assert_eq!(
            render_child_directory_locator(
                "{project-key}/{native-session-id}/subagents",
                b"project-a",
                b"session-7",
            )
            .unwrap(),
            PathBuf::from("project-a/session-7/subagents")
        );

        let plan = child_directory_scope_plan("{project-key}/{native-session-id}/subagents");
        let reserved_identity = [
            ScopeIdentityInput {
                name: "project-key",
                value: b"project-a",
            },
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"session-7",
            },
        ];
        let reservation = plan
            .reserve(ScopeAccessRequest {
                relation_id: "descendant-transcripts",
                operation: AccessOperation::ObjectListing,
                phase: AccessPhase::Revalidation,
                parent_token: None,
                identity_inputs: &reserved_identity,
                depth: 1,
                max_bytes: 1,
                max_rows: 0,
            })
            .unwrap();
        let substituted_identity = [
            reserved_identity[0],
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"other-session",
            },
        ];
        assert!(reservation
            .render_child_directory_locator(&substituted_identity)
            .is_err());

        for invalid in [
            b"..".as_slice(),
            b"nested/project".as_slice(),
            b"nested\\project".as_slice(),
            b"line\nbreak".as_slice(),
            b"\xff".as_slice(),
        ] {
            assert!(render_child_directory_locator(
                "{project-key}/{native-session-id}/subagents",
                invalid,
                b"session-7",
            )
            .is_err());
        }

        let artifact_plan = artifact_scope_plan("file-history/{native-session-id}");
        let artifact_identity = [
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"session-7",
            },
            ScopeIdentityInput {
                name: "backup-name",
                value: b"backup-a",
            },
            ScopeIdentityInput {
                name: "artifact-version",
                value: b"9",
            },
        ];
        let artifact_reservation = artifact_plan
            .reserve(ScopeAccessRequest {
                relation_id: "summary-sidecar",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Revalidation,
                parent_token: None,
                identity_inputs: &artifact_identity,
                depth: 1,
                max_bytes: 1,
                max_rows: 0,
            })
            .unwrap();
        assert!(artifact_reservation
            .render_child_directory_locator(&artifact_identity)
            .is_err());
    }

    #[test]
    fn directory_entry_reservations_bind_root_parent_fanout_and_depth() {
        let identity = [
            ScopeIdentityInput {
                name: "project-key",
                value: b"project-a",
            },
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"session-7",
            },
        ];
        let reserve_root = |plan: &ScopeAccessPlan| {
            plan.reserve(ScopeAccessRequest {
                relation_id: "descendant-transcripts",
                operation: AccessOperation::ObjectListing,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 1,
                max_rows: 0,
            })
            .unwrap()
        };
        let child_a = confined_relative_path_key(Path::new("child-a.jsonl")).unwrap();
        let child_b = confined_relative_path_key(Path::new("child-b.jsonl")).unwrap();
        let child_c = confined_relative_path_key(Path::new("child-c.jsonl")).unwrap();

        let plan = child_directory_scope_plan("{project-key}/{native-session-id}/subagents");
        let root = reserve_root(&plan);
        let authority = root.directory_root_authority().unwrap();
        let root_token = root.object_token();
        let first = root
            .reserve_directory_entry(&authority, &child_a, None, 1)
            .unwrap();
        let first_token = first.object_token();
        first.complete(true).unwrap();
        root.reserve_directory_entry(&authority, &child_b, None, 1)
            .unwrap()
            .complete(true)
            .unwrap();
        let error = root
            .reserve_directory_entry(&authority, &child_c, None, 1)
            .unwrap_err();
        assert!(matches!(
            error,
            AccessBudgetError::LimitExceeded {
                limit: AccessLimit::MaxFanOut,
                ..
            }
        ));
        root.complete(1, 0, AccessOutcome::Available).unwrap();
        let snapshot = plan.snapshot("descendant-transcripts").unwrap();
        assert_eq!(snapshot.attempts, 4);
        assert_eq!(snapshot.objects_accessed, 3);
        assert_eq!(snapshot.denied, 1);
        assert_eq!(snapshot.trace[0].parent_token, None);
        assert_eq!(snapshot.trace[1].parent_token, Some(root_token));
        assert_eq!(snapshot.trace[2].parent_token, Some(root_token));

        let replay_plan = child_directory_scope_plan("{project-key}/{native-session-id}/subagents");
        let replay_root = reserve_root(&replay_plan);
        let replay_authority = replay_root.directory_root_authority().unwrap();
        assert!(matches!(
            replay_root.reserve_directory_entry(&authority, &child_a, None, 1),
            Err(AccessBudgetError::InvalidConfig(_))
        ));
        assert_eq!(
            replay_root
                .reserve_directory_entry(&replay_authority, &child_a, None, 1)
                .unwrap()
                .object_token(),
            first_token
        );

        let nested = confined_relative_path_key(Path::new("child-a.jsonl/nested")).unwrap();
        let depth_error = replay_root
            .reserve_directory_entry(&replay_authority, &nested, Some(&child_a), 3)
            .unwrap_err();
        assert!(matches!(
            depth_error,
            AccessBudgetError::LimitExceeded {
                limit: AccessLimit::MaxDepth,
                ..
            }
        ));
        replay_root.fail_conservative();
    }

    #[test]
    fn completed_directory_listing_mints_one_shot_bounded_member_reads() {
        let identity = [
            ScopeIdentityInput {
                name: "project-key",
                value: b"project-a",
            },
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"session-7",
            },
        ];
        let reserve_root = |plan: &ScopeAccessPlan| {
            plan.reserve(ScopeAccessRequest {
                relation_id: "descendant-transcripts",
                operation: AccessOperation::ObjectListing,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 1,
                max_rows: 0,
            })
            .unwrap()
        };
        let child_key = confined_relative_path_key(Path::new("child.jsonl")).unwrap();
        let ignored_key = confined_relative_path_key(Path::new("ignored.tmp")).unwrap();

        let plan = child_directory_scope_plan("{project-key}/{native-session-id}/subagents");
        let root = reserve_root(&plan);
        let root_token = root.object_token();
        let authority = root.directory_root_authority().unwrap();
        let child = root
            .reserve_directory_entry(&authority, &child_key, None, 1)
            .unwrap();
        let child_token = child.object_token();
        child.complete(true).unwrap();
        let ignored = root
            .reserve_directory_entry(&authority, &ignored_key, None, 1)
            .unwrap();
        let ignored_token = ignored.object_token();
        ignored.complete(false).unwrap();
        let read_authority = root
            .complete_directory_listing(&authority, 16, AccessOutcome::Available)
            .unwrap()
            .unwrap();
        assert_eq!(read_authority.max_object_bytes(), 16);

        let read = read_authority
            .reserve_member_read(child_token, root_token, 1)
            .unwrap();
        assert_eq!(read.max_object_bytes(), 16);
        read.complete(5, AccessOutcome::Available).unwrap();
        assert!(matches!(
            read_authority.reserve_member_read(child_token, root_token, 1),
            Err(AccessBudgetError::InvalidConfig(_))
        ));
        assert!(matches!(
            read_authority.reserve_member_read(ignored_token, root_token, 1),
            Err(AccessBudgetError::InvalidConfig(_))
        ));
        let fabricated =
            AccessObjectToken::derive("descendant-transcripts", &[b"fabricated"]).unwrap();
        assert!(matches!(
            read_authority.reserve_member_read(fabricated, root_token, 1),
            Err(AccessBudgetError::InvalidConfig(_))
        ));

        let snapshot = plan.snapshot("descendant-transcripts").unwrap();
        assert_eq!(snapshot.attempts, 4);
        assert_eq!(snapshot.completed, 4);
        assert_eq!(snapshot.bytes_read, 5);
        assert_eq!(snapshot.trace[3].operation, AccessOperation::ObjectRead);
        assert_eq!(snapshot.trace[3].reserved_bytes, 16);
        assert_eq!(snapshot.trace[3].parent_token, Some(root_token));

        let unavailable_plan =
            child_directory_scope_plan("{project-key}/{native-session-id}/subagents");
        let unavailable_root = reserve_root(&unavailable_plan);
        let unavailable_authority = unavailable_root.directory_root_authority().unwrap();
        assert!(unavailable_root
            .complete_directory_listing(&unavailable_authority, 16, AccessOutcome::Unavailable,)
            .unwrap()
            .is_none());
    }

    #[test]
    fn related_object_locators_are_confined_and_bound_to_the_reserved_identity() {
        let sibling_plan = related_object_scope_plan(
            ScopeRelationPrimitive::SiblingObject,
            "{actor-transcript}.meta.json",
            &["actor-transcript"],
        );
        let sibling_identity = [ScopeIdentityInput {
            name: "actor-transcript",
            value: b"agent-worker",
        }];
        assert_eq!(
            reserve_related_object(&sibling_plan, &sibling_identity)
                .render_related_object_locator(&sibling_identity)
                .unwrap(),
            PathBuf::from("agent-worker.meta.json")
        );

        let reference_plan = related_object_scope_plan(
            ScopeRelationPrimitive::ReferencedObjectFromField,
            "{team}/inboxes/{recipient}.json",
            &["team", "recipient"],
        );
        let reference_identity = [
            ScopeIdentityInput {
                name: "team",
                value: b"alpha",
            },
            ScopeIdentityInput {
                name: "recipient",
                value: b"team-lead",
            },
        ];
        let reservation = reserve_related_object(&reference_plan, &reference_identity);
        assert_eq!(
            reservation
                .render_related_object_locator(&reference_identity)
                .unwrap(),
            PathBuf::from("alpha/inboxes/team-lead.json")
        );

        let substituted_identity = [
            ScopeIdentityInput {
                name: "team",
                value: b"other-team",
            },
            reference_identity[1],
        ];
        assert!(reservation
            .render_related_object_locator(&substituted_identity)
            .is_err());

        for invalid in [
            b"..".as_slice(),
            b"nested/team".as_slice(),
            b"nested\\team".as_slice(),
            b"line\nbreak".as_slice(),
            b"\xff".as_slice(),
        ] {
            let identity = [
                ScopeIdentityInput {
                    name: "team",
                    value: invalid,
                },
                reference_identity[1],
            ];
            assert!(reserve_related_object(&reference_plan, &identity)
                .render_related_object_locator(&identity)
                .is_err());
        }

        let child_plan = child_directory_scope_plan("{project-key}/{native-session-id}");
        let child_identity = [
            ScopeIdentityInput {
                name: "project-key",
                value: b"project-a",
            },
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"session-7",
            },
        ];
        assert!(child_plan
            .reserve(ScopeAccessRequest {
                relation_id: "descendant-transcripts",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Revalidation,
                parent_token: None,
                identity_inputs: &child_identity,
                depth: 1,
                max_bytes: 1,
                max_rows: 0,
            })
            .unwrap()
            .render_related_object_locator(&child_identity)
            .is_err());
    }

    #[test]
    fn artifact_locator_templates_render_only_exact_bound_identity_inputs() {
        assert_eq!(
            render_artifact_locator(
                "file-history/{native-session-id}/{backup-name}.{artifact-version}",
                b"session-7",
                b"backup-a",
                b"9",
            )
            .unwrap(),
            PathBuf::from("file-history/session-7/backup-a.9")
        );

        let plan = artifact_scope_plan("file-history/{native-session-id}/{backup-name}");
        let reserved_identity = [
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"session-7",
            },
            ScopeIdentityInput {
                name: "backup-name",
                value: b"backup-a",
            },
            ScopeIdentityInput {
                name: "artifact-version",
                value: b"9",
            },
        ];
        let reservation = plan
            .reserve(ScopeAccessRequest {
                relation_id: "summary-sidecar",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Revalidation,
                parent_token: None,
                identity_inputs: &reserved_identity,
                depth: 1,
                max_bytes: 1,
                max_rows: 0,
            })
            .unwrap();
        let substituted_identity = [
            ScopeIdentityInput {
                name: "native-session-id",
                value: b"other-session",
            },
            reserved_identity[1],
            reserved_identity[2],
        ];
        assert!(reservation
            .render_evidence_locator(&substituted_identity)
            .is_err());
    }

    #[test]
    fn artifact_locator_templates_reject_conceptual_or_ambiguous_shapes() {
        for locator in [
            "declared-artifact-locator",
            "file-history/{unknown-input}",
            "file-history/{backup-name}/{backup-name}",
            "file-history/{backup-name",
            "file-history/backup-name}",
            "file-history/{{backup-name}}",
        ] {
            let plan = artifact_scope_plan(locator);
            let declaration = plan.relation("summary-sidecar").unwrap();
            assert!(
                validate_evidence_locator_template(declaration).is_err(),
                "accepted {locator:?}"
            );
        }
    }

    #[test]
    fn artifact_locator_rendering_is_confined_and_bounded_before_retention() {
        for native_session_id in [
            b"..".as_slice(),
            b".".as_slice(),
            b"C:".as_slice(),
            b"nested/session".as_slice(),
            b"nested\\session".as_slice(),
            b"line\nbreak".as_slice(),
            b"\xff".as_slice(),
        ] {
            assert!(
                render_artifact_locator(
                    "{native-session-id}/{backup-name}",
                    native_session_id,
                    b"backup-a",
                    b"9",
                )
                .is_err(),
                "accepted {native_session_id:?}"
            );
        }

        let exact = vec![b'a'; MAX_RENDERED_SCOPE_LOCATOR_BYTES];
        assert_eq!(
            render_artifact_locator("{backup-name}", b"session-7", &exact, b"9")
                .unwrap()
                .as_os_str()
                .len(),
            MAX_RENDERED_SCOPE_LOCATOR_BYTES
        );
        let oversized = vec![b'a'; MAX_RENDERED_SCOPE_LOCATOR_BYTES + 1];
        assert!(render_artifact_locator("{backup-name}", b"session-7", &oversized, b"9").is_err());
    }

    #[test]
    fn scope_program_compiles_exact_relation_budgets_and_returns_declared_locator() {
        let plan = ScopeAccessPlan::for_program(&grok_scope_manifest(), "observe-session-sidecars")
            .unwrap();
        assert_eq!(plan.adapter_id(), "grok");
        assert_eq!(plan.status(), ScopeProgramStatus::Incomplete);
        assert_eq!(plan.snapshots().len(), 4);
        assert_eq!(
            plan.snapshot("summary-sidecar").unwrap().bounds,
            ScopeAccessBounds {
                max_fan_out: 1,
                max_depth: 1,
                max_objects: 1,
                max_bytes: 1024 * 1024,
                max_rows: 0,
            }
        );

        let identity = [ScopeIdentityInput {
            name: "history-object",
            value: b"session/history.jsonl",
        }];
        let reservation = plan
            .reserve(ScopeAccessRequest {
                relation_id: "summary-sidecar",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 1024,
                max_rows: 0,
            })
            .unwrap();
        assert_eq!(reservation.access_root(), "sessions");
        assert_eq!(reservation.locator(), "summary.json");
        assert_eq!(
            reservation.unavailable_behavior(),
            ScopeUnavailableBehavior::SkipOptional
        );
        reservation
            .complete(512, 0, AccessOutcome::Available)
            .unwrap();
        let snapshot = plan.snapshot("summary-sidecar").unwrap();
        assert_eq!(snapshot.objects_accessed, 1);
        assert_eq!(snapshot.bytes_read, 512);
    }

    #[test]
    fn scope_program_denies_unknown_operations_and_identity_mismatches_before_budget_access() {
        let plan = ScopeAccessPlan::for_program(&grok_scope_manifest(), "observe-session-sidecars")
            .unwrap();
        let identity = [ScopeIdentityInput {
            name: "history-object",
            value: b"session/history.jsonl",
        }];
        let request = |relation_id, operation, identity_inputs| ScopeAccessRequest {
            relation_id,
            operation,
            phase: AccessPhase::Initial,
            parent_token: None,
            identity_inputs,
            depth: 1,
            max_bytes: 1,
            max_rows: u64::from(operation == AccessOperation::ParameterizedQuery),
        };

        assert!(matches!(
            plan.reserve(request(
                "missing-relation",
                AccessOperation::ObjectRead,
                &identity
            )),
            Err(AccessBudgetError::ScopeDenied {
                reason: ScopeAccessDenial::UnknownRelation,
                ..
            })
        ));
        assert!(matches!(
            plan.reserve(request(
                "summary-sidecar",
                AccessOperation::ParameterizedQuery,
                &identity
            )),
            Err(AccessBudgetError::ScopeDenied {
                reason: ScopeAccessDenial::OperationNotAllowed,
                ..
            })
        ));
        let wrong_identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"session",
        }];
        assert!(matches!(
            plan.reserve(request(
                "summary-sidecar",
                AccessOperation::ObjectRead,
                &wrong_identity
            )),
            Err(AccessBudgetError::ScopeDenied {
                reason: ScopeAccessDenial::IdentityInputsMismatch,
                ..
            })
        ));
        assert_eq!(plan.snapshot("summary-sidecar").unwrap().attempts, 0);

        assert!(matches!(
            plan.reserve(ScopeAccessRequest {
                relation_id: "summary-sidecar",
                operation: AccessOperation::ObjectRead,
                phase: AccessPhase::Initial,
                parent_token: None,
                identity_inputs: &identity,
                depth: 1,
                max_bytes: 1024 * 1024 + 1,
                max_rows: 0,
            }),
            Err(AccessBudgetError::LimitExceeded {
                limit: AccessLimit::MaxBytes,
                ..
            })
        ));
        let snapshot = plan.snapshot("summary-sidecar").unwrap();
        assert_eq!(snapshot.attempts, 1);
        assert_eq!(snapshot.denied, 1);
        assert_eq!(snapshot.objects_accessed, 0);
    }

    #[test]
    fn scope_relation_operation_mapping_is_closed() {
        let operations = [
            AccessOperation::ObjectRead,
            AccessOperation::ParameterizedQuery,
            AccessOperation::ObjectListing,
        ];
        let cases: &[(ScopeRelationPrimitive, &[AccessOperation])] = &[
            (
                ScopeRelationPrimitive::KnownObject,
                &[AccessOperation::ObjectRead],
            ),
            (
                ScopeRelationPrimitive::SiblingObject,
                &[AccessOperation::ObjectRead],
            ),
            (
                ScopeRelationPrimitive::ChildDirectoryByNativeId,
                &[AccessOperation::ObjectRead, AccessOperation::ObjectListing],
            ),
            (
                ScopeRelationPrimitive::ReferencedObjectFromField,
                &[AccessOperation::ObjectRead],
            ),
            (
                ScopeRelationPrimitive::BoundedIndexLookup,
                &[AccessOperation::ObjectRead],
            ),
            (
                ScopeRelationPrimitive::ParameterizedSQLiteRows,
                &[AccessOperation::ParameterizedQuery],
            ),
            (
                ScopeRelationPrimitive::KeyNamespace,
                &[AccessOperation::ObjectRead, AccessOperation::ObjectListing],
            ),
            (
                ScopeRelationPrimitive::ArtifactLocatorFromEvidence,
                &[AccessOperation::ObjectRead],
            ),
        ];
        for (primitive, allowed) in cases {
            for operation in operations {
                assert_eq!(
                    primitive_allows_operation(*primitive, operation),
                    allowed.contains(&operation),
                    "{primitive:?} / {operation:?}"
                );
            }
        }
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
