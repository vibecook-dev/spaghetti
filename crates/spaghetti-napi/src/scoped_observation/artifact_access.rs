//! Non-I/O artifact relation authorization.
//!
//! This seam consumes current admitted artifact metadata only far enough to
//! reserve one exact promoted `ArtifactLocatorFromEvidence` relation and
//! render its declaration template from already-bound identity evidence. It
//! retains only a confined relative path; it does not join a native root, open
//! an object, or claim artifact availability.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::adapter::{ContractCompleteness, QualifiedValueQuality, ScopeRelationPrimitive};
use crate::source::{
    validate_evidence_locator_template, AccessBudgetError, AccessObjectToken, AccessOperation,
    AccessPhase, AuthorizedScopeAccessPlan, ScopeAccessRequest, ScopeAccessReservation,
    ScopeIdentityInput,
};

use super::artifact_wire::ScopedValidatedArtifactReadCommand;
use super::{
    ScopedAccessRootGrant, ScopedArtifactRelationGrant, ScopedKnownObjectGrant,
    ScopedObservationAccessError, ScopedObservationAccessPass,
};

const ARTIFACT_IDENTITY_INPUTS: [&str; 3] =
    ["native-session-id", "backup-name", "artifact-version"];

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScopedArtifactRelationAccessError {
    #[error("artifact relation proof does not match the active attachment and pass")]
    InvalidBinding,
    #[error("artifact relation requires an exact complete native session identity")]
    NativeSessionUnavailable,
    #[error(transparent)]
    Access(#[from] AccessBudgetError),
}

/// One exact relation reservation whose native inputs remain private. The
/// proof borrows both the validated active epoch and the pass so neither can
/// be replaced while a future locator mediator consumes it. Dropping this
/// value conservatively abandons the common access reservation.
pub(crate) struct ScopedArtifactRelationReservation<'command, 'pass> {
    _validated: ScopedValidatedArtifactReadCommand<'command>,
    _pass: &'pass ScopedObservationAccessPass,
    _reservation: ScopeAccessReservation,
    relation_id: Arc<str>,
    access_root: Arc<str>,
    locator_id: Arc<str>,
    artifact_kind: Arc<str>,
    object_token: AccessObjectToken,
    max_bytes: u64,
    _root: PathBuf,
    _relative_path: PathBuf,
    _native_session_id: Arc<str>,
    _native_artifact_id: Arc<str>,
    _artifact_version: Arc<str>,
}

impl fmt::Debug for ScopedArtifactRelationReservation<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedArtifactRelationReservation")
            .field("relation_id", &self.relation_id)
            .field("access_root", &self.access_root)
            .field("locator_id", &self.locator_id)
            .field("artifact_kind", &self.artifact_kind)
            .field("object_token", &self.object_token)
            .field("max_bytes", &self.max_bytes)
            .field("native_root", &"<redacted>")
            .field("identity_inputs", &"<redacted>")
            .finish_non_exhaustive()
    }
}

pub(super) fn validate_artifact_relation_grants(
    plan: &AuthorizedScopeAccessPlan,
    grants: Vec<ScopedArtifactRelationGrant>,
) -> Result<BTreeMap<String, Arc<str>>, ScopedObservationAccessError> {
    let mut validated = BTreeMap::new();
    let mut relation_ids = BTreeSet::new();
    for grant in grants {
        super::artifact_wire::validate_artifact_kind(&grant.artifact_kind)
            .map_err(|error| ScopedObservationAccessError::InvalidGrant(error.to_string()))?;
        let relation = plan.relation(&grant.relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(format!(
                "artifact relation {:?} is absent from the authorized program",
                grant.relation_id
            ))
        })?;
        if relation.primitive != ScopeRelationPrimitive::ArtifactLocatorFromEvidence
            || relation
                .identity_inputs
                .iter()
                .map(String::as_str)
                .ne(ARTIFACT_IDENTITY_INPUTS)
            || validate_evidence_locator_template(relation).is_err()
        {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "relation {:?} is not an exact evidence-derived artifact relation",
                grant.relation_id
            )));
        }
        if !relation_ids.insert(grant.relation_id.clone()) {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "artifact relation {:?} is selected more than once",
                grant.relation_id
            )));
        }
        if validated
            .insert(grant.artifact_kind.clone(), Arc::from(grant.relation_id))
            .is_some()
        {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "duplicate artifact-kind relation selection for {:?}",
                grant.artifact_kind
            )));
        }
    }
    Ok(validated)
}

pub(super) fn validate_access_root_grants(
    plan: &AuthorizedScopeAccessPlan,
    known_objects: &BTreeMap<String, ScopedKnownObjectGrant>,
    artifact_relations: &BTreeMap<String, Arc<str>>,
    grants: Vec<ScopedAccessRootGrant>,
) -> Result<BTreeMap<String, ScopedAccessRootGrant>, ScopedObservationAccessError> {
    let mut expected = known_objects
        .values()
        .map(|grant| grant.access_root.as_str())
        .collect::<BTreeSet<_>>();
    for relation_id in artifact_relations.values() {
        let relation = plan.relation(relation_id).ok_or_else(|| {
            ScopedObservationAccessError::InvalidGrant(
                "selected artifact relation disappeared from the authorized plan".to_string(),
            )
        })?;
        expected.insert(relation.access_root.as_str());
    }
    if grants.len() != expected.len() {
        return Err(ScopedObservationAccessError::InvalidGrant(
            "the host-approved access-root set must equal every selected relation root".to_string(),
        ));
    }

    let mut validated = BTreeMap::new();
    for grant in grants {
        if !expected.contains(grant.access_root.as_str())
            || grant.root.as_os_str().is_empty()
            || !grant.root.is_absolute()
        {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "access root {:?} is not an exact selected absolute root",
                grant.access_root
            )));
        }
        let access_root = grant.access_root.clone();
        if validated.insert(access_root.clone(), grant).is_some() {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "duplicate host-approved access root {access_root:?}"
            )));
        }
    }
    if validated
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected
    {
        return Err(ScopedObservationAccessError::InvalidGrant(
            "the host-approved access-root set omits or adds a selected relation root".to_string(),
        ));
    }
    for known in known_objects.values() {
        if validated
            .get(&known.access_root)
            .is_none_or(|root| root.root != known.root)
        {
            return Err(ScopedObservationAccessError::InvalidGrant(format!(
                "known-object relation {:?} does not use its exact host-approved access root",
                known.relation_id
            )));
        }
    }
    Ok(validated)
}

impl ScopedObservationAccessPass {
    /// Reserve the exact selected artifact relation from a command whose
    /// evidence is still current. The fixed root edge and identity-input names
    /// leave no caller-controlled topology or native value at this boundary.
    pub(crate) fn reserve_artifact_relation_from_evidence<'command, 'pass>(
        &'pass self,
        validated: ScopedValidatedArtifactReadCommand<'command>,
    ) -> Result<ScopedArtifactRelationReservation<'command, 'pass>, ScopedArtifactRelationAccessError>
    {
        if self.state.closed.load(std::sync::atomic::Ordering::Acquire)
            || !Arc::ptr_eq(
                &self.attachment_authority,
                &validated.command.attachment_authority,
            )
            || self.root_identity != validated.command.root
        {
            return Err(ScopedArtifactRelationAccessError::InvalidBinding);
        }
        let relation_id = validated
            .command
            .artifact_relation_id
            .as_deref()
            .ok_or(ScopedArtifactRelationAccessError::InvalidBinding)?;
        if self
            .artifact_relations
            .get(&validated.command.artifact_kind)
            .is_none_or(|selected| selected.as_ref() != relation_id)
        {
            return Err(ScopedArtifactRelationAccessError::InvalidBinding);
        }
        let relation = self
            .plan
            .relation(relation_id)
            .ok_or(ScopedArtifactRelationAccessError::InvalidBinding)?;
        let root = self
            .access_roots
            .get(&relation.access_root)
            .ok_or(ScopedArtifactRelationAccessError::InvalidBinding)?;
        let claim = self
            .root_identity
            .native_session_claim
            .as_ref()
            .ok_or(ScopedArtifactRelationAccessError::NativeSessionUnavailable)?;
        let native_session = claim
            .identity
            .value
            .as_ref()
            .filter(|_| {
                matches!(
                    claim.identity.quality,
                    QualifiedValueQuality::Exact | QualifiedValueQuality::NativeClaimed
                ) && claim.identity.completeness == ContractCompleteness::Complete
            })
            .ok_or(ScopedArtifactRelationAccessError::NativeSessionUnavailable)?;
        let evidence = validated
            .command
            .artifact_evidence
            .as_ref()
            .ok_or(ScopedArtifactRelationAccessError::InvalidBinding)?;
        let artifact_version: Arc<str> = Arc::from(evidence.version().to_string());
        let identity_inputs = [
            ScopeIdentityInput {
                name: ARTIFACT_IDENTITY_INPUTS[0],
                value: native_session.native_id.as_bytes(),
            },
            ScopeIdentityInput {
                name: ARTIFACT_IDENTITY_INPUTS[1],
                value: evidence.native_artifact_id().as_bytes(),
            },
            ScopeIdentityInput {
                name: ARTIFACT_IDENTITY_INPUTS[2],
                value: artifact_version.as_bytes(),
            },
        ];
        let reservation = self.plan.reserve(ScopeAccessRequest {
            relation_id,
            operation: AccessOperation::ObjectRead,
            phase: AccessPhase::Revalidation,
            parent_token: None,
            identity_inputs: &identity_inputs,
            depth: 1,
            max_bytes: validated.command.max_bytes,
            max_rows: 0,
        })?;
        if reservation.primitive() != ScopeRelationPrimitive::ArtifactLocatorFromEvidence
            || reservation.access_root() != root.access_root
            || reservation.locator() != relation.locator
        {
            reservation.fail_conservative();
            return Err(ScopedArtifactRelationAccessError::InvalidBinding);
        }
        let relative_path = match reservation.render_evidence_locator(&identity_inputs) {
            Ok(relative_path) => relative_path,
            Err(error) => {
                reservation.fail_conservative();
                return Err(ScopedArtifactRelationAccessError::Access(error));
            }
        };
        let object_token = reservation.object_token();
        Ok(ScopedArtifactRelationReservation {
            relation_id: Arc::from(relation_id),
            access_root: Arc::from(reservation.access_root()),
            locator_id: Arc::from(reservation.locator()),
            artifact_kind: Arc::from(validated.command.artifact_kind.as_str()),
            object_token,
            max_bytes: validated.command.max_bytes,
            _root: root.root.clone(),
            _relative_path: relative_path,
            _native_session_id: Arc::from(native_session.native_id.as_str()),
            _native_artifact_id: Arc::clone(evidence.native_artifact_id()),
            _artifact_version: artifact_version,
            _validated: validated,
            _pass: self,
            _reservation: reservation,
        })
    }
}

#[cfg(test)]
mod tests;
