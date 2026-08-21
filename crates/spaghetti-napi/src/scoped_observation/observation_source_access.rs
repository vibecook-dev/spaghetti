//! Attachment-owned binding for authorized dynamic observation sources.
//!
//! This module still performs no native I/O. It joins the declaration/runtime
//! stream proof to the exact source-instance root already approved by the
//! scoped attachment, while retaining the access reservation and borrowing the
//! pass that owns it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::adapter::{
    AdapterId, CanonicalSourceInstanceKey, CoverageObjectKey, CoverageStreamKey,
    ScopeRelationPrimitive, SourceInstance, SourceInstanceKey, StreamSpec,
};
use crate::source::{
    AccessBudgetError, AccessObjectToken, AccessOperation, AccessOutcome,
    AuthorizedObservationRuntimeStreamReservation, DirectoryEntryKind, DirectorySelection,
    DirectorySnapshot, DirectorySnapshotConfig, GlobPattern, ScopeAccessRequest,
};

use super::{ScopedAccessRootGrant, ScopedObservationAccessPass, ScopedSourceObjectIdentity};

const MEMBERSHIP_STREAM_DOMAIN: &[u8] = b"spaghetti/rfc012d/scope-relation-membership-stream/v1\0";
const MEMBERSHIP_OBJECT_NAMESPACE: &str = "spaghetti.scope-relation-membership-v1";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScopedObservationRuntimeSourceError {
    #[error("scoped observation source binding does not match the active attachment")]
    InvalidBinding,
    #[error("scoped observation host closed before source binding completed")]
    Closed,
    #[error(transparent)]
    Access(#[from] AccessBudgetError),
}

/// Exact source-instance/root join underneath one runtime stream reservation.
/// Native root and relative locator remain separate so no unconfined joined
/// path can escape this private boundary.
pub(crate) struct ScopedObservationRuntimeSourceBinding {
    runtime: AuthorizedObservationRuntimeStreamReservation,
    root: PathBuf,
    canonical_source_instance_key: CanonicalSourceInstanceKey,
}

impl fmt::Debug for ScopedObservationRuntimeSourceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationRuntimeSourceBinding")
            .field("has_source_instance", &true)
            .field("has_native_root", &true)
            .field(
                "has_relative_selector",
                &self.runtime.relative_selector().is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ScopedObservationRuntimeSourceBinding {
    fn bind(
        runtime: AuthorizedObservationRuntimeStreamReservation,
        instance: &SourceInstance,
        approved_root: &ScopedAccessRootGrant,
        expected_source_instance_key: &CanonicalSourceInstanceKey,
    ) -> Result<Self, ScopedObservationRuntimeSourceError> {
        let canonical_source_instance_key = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        );
        let matches = canonical_source_instance_key
            .as_ref()
            .is_ok_and(|actual| actual == expected_source_instance_key)
            && instance.id == runtime.source_instance_id()
            && instance.spec.identity_contract_version
                == runtime.source_instance_identity_contract_version()
            && &instance.spec.stable_key == runtime.source_instance_key()
            && approved_root.access_root == runtime.access_root()
            && !approved_root.root.as_os_str().is_empty()
            && approved_root.root.is_absolute()
            && instance
                .spec
                .roots
                .iter()
                .filter(|root| root.name == runtime.access_root())
                .count()
                == 1
            && instance
                .root(runtime.access_root())
                .is_ok_and(|root| root == approved_root.root);
        if !matches {
            runtime.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        Ok(Self {
            runtime,
            root: approved_root.root.clone(),
            canonical_source_instance_key: *expected_source_instance_key,
        })
    }

    pub(crate) fn relation_id(&self) -> &str {
        self.runtime.relation_id()
    }

    pub(crate) fn access_root(&self) -> &str {
        self.runtime.access_root()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn locator(&self) -> &Path {
        self.runtime.locator()
    }

    pub(crate) fn relative_selector(&self) -> Option<&str> {
        self.runtime.relative_selector()
    }

    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.runtime.object_token()
    }

    pub(crate) fn source_instance_id(&self) -> u64 {
        self.runtime.source_instance_id()
    }

    pub(crate) fn source_instance_key(&self) -> &SourceInstanceKey {
        self.runtime.source_instance_key()
    }

    pub(crate) fn canonical_source_instance_key(&self) -> CanonicalSourceInstanceKey {
        self.canonical_source_instance_key
    }

    pub(crate) fn stream(&self) -> &StreamSpec {
        self.runtime.stream()
    }

    pub(crate) fn complete(
        self,
        bytes_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.runtime.complete(bytes_read, outcome)
    }

    pub(crate) fn fail_conservative(self) {
        self.runtime.fail_conservative();
    }
}

/// Pass-borrowed owner of one exact dynamic source binding. The attachment's
/// approved root cannot be replaced after this value is created, and dropping
/// the value still conservatively consumes its common reservation.
pub(crate) struct ScopedObservationRuntimeSourceReservation<'pass> {
    _pass: &'pass ScopedObservationAccessPass,
    binding: ScopedObservationRuntimeSourceBinding,
}

/// Pre-I/O directory-membership authority compiled only from one exact
/// attachment-owned child-directory reservation. The selector and scan bounds
/// are no longer caller inputs, and the membership source identity is derived
/// without retaining a native path.
pub(crate) struct ScopedObservationDirectoryMembershipContract<'pass> {
    reservation: ScopedObservationRuntimeSourceReservation<'pass>,
    proof: ScopedObservationDirectoryMembershipProof,
}

pub(crate) struct ScopedObservationDirectoryMembershipProof {
    config: DirectorySnapshotConfig,
    selector: GlobPattern,
    source: ScopedSourceObjectIdentity,
}

impl fmt::Debug for ScopedObservationDirectoryMembershipContract<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.proof.fmt(formatter)
    }
}

impl fmt::Debug for ScopedObservationDirectoryMembershipProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMembershipContract")
            .field("max_entries", &self.config.max_entries)
            .field(
                "max_entries_per_directory",
                &self.config.max_entries_per_directory,
            )
            .field("max_depth", &self.config.max_depth)
            .field("has_relative_selector", &true)
            .field("has_membership_source", &true)
            .finish_non_exhaustive()
    }
}

impl ScopedObservationDirectoryMembershipContract<'_> {
    pub(crate) fn config(&self) -> &DirectorySnapshotConfig {
        self.proof.config()
    }

    pub(crate) fn source(&self) -> &ScopedSourceObjectIdentity {
        self.proof.source()
    }

    pub(crate) fn select(
        &self,
        relative_path: &Path,
        kind: DirectoryEntryKind,
    ) -> DirectorySelection {
        self.proof.select(relative_path, kind)
    }

    pub(crate) fn fail_conservative(self) {
        self.reservation.fail_conservative();
    }
}

impl ScopedObservationDirectoryMembershipProof {
    pub(crate) fn config(&self) -> &DirectorySnapshotConfig {
        &self.config
    }

    pub(crate) fn source(&self) -> &ScopedSourceObjectIdentity {
        &self.source
    }

    pub(crate) fn select(
        &self,
        relative_path: &Path,
        kind: DirectoryEntryKind,
    ) -> DirectorySelection {
        match kind {
            DirectoryEntryKind::Directory => DirectorySelection::Recurse,
            DirectoryEntryKind::File if self.selector.matches_path(relative_path) => {
                DirectorySelection::Include
            }
            DirectoryEntryKind::File => DirectorySelection::Ignore,
        }
    }
}

impl fmt::Debug for ScopedObservationRuntimeSourceReservation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.binding.fmt(formatter)
    }
}

impl<'pass> ScopedObservationRuntimeSourceReservation<'pass> {
    pub(crate) fn binding(&self) -> &ScopedObservationRuntimeSourceBinding {
        &self.binding
    }

    pub(crate) fn complete(
        self,
        bytes_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.binding.complete(bytes_read, outcome)
    }

    pub(crate) fn fail_conservative(self) {
        self.binding.fail_conservative();
    }

    pub(crate) fn into_directory_membership_contract(
        self,
    ) -> Result<
        ScopedObservationDirectoryMembershipContract<'pass>,
        ScopedObservationRuntimeSourceError,
    > {
        let prepared = prepare_directory_membership_contract(self.binding());
        match prepared {
            Ok(proof) => Ok(ScopedObservationDirectoryMembershipContract {
                reservation: self,
                proof,
            }),
            Err(error) => {
                self.fail_conservative();
                Err(error)
            }
        }
    }
}

fn prepare_directory_membership_contract(
    binding: &ScopedObservationRuntimeSourceBinding,
) -> Result<ScopedObservationDirectoryMembershipProof, ScopedObservationRuntimeSourceError> {
    if binding.runtime.primitive() != ScopeRelationPrimitive::ChildDirectoryByNativeId
        || binding.runtime.operation() != AccessOperation::ObjectListing
    {
        return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
    }
    let bounds = binding.runtime.bounds();
    let config = DirectorySnapshotConfig {
        max_entries: usize::try_from(bounds.max_objects)
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
        max_entries_per_directory: usize::try_from(bounds.max_fan_out)
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
        max_depth: usize::try_from(bounds.max_depth)
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
    };
    DirectorySnapshot::new(config.clone())
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let selector = binding
        .relative_selector()
        .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)
        .and_then(|selector| {
            GlobPattern::new(selector)
                .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)
        })?;
    let adapter_id = AdapterId::new(binding.runtime.adapter_id())
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let mut membership_stream_key =
        Vec::with_capacity(MEMBERSHIP_STREAM_DOMAIN.len() + 8 + binding.runtime.program_id().len());
    membership_stream_key.extend_from_slice(MEMBERSHIP_STREAM_DOMAIN);
    membership_stream_key
        .extend_from_slice(&(binding.runtime.program_id().len() as u64).to_be_bytes());
    membership_stream_key.extend_from_slice(binding.runtime.program_id().as_bytes());
    let stream_key = CoverageStreamKey::derive(adapter_id.as_str(), &membership_stream_key)
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let object_key = CoverageObjectKey::derive(
        MEMBERSHIP_OBJECT_NAMESPACE,
        binding.object_token().as_bytes(),
    )
    .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    Ok(ScopedObservationDirectoryMembershipProof {
        config,
        selector,
        source: ScopedSourceObjectIdentity {
            adapter_id,
            source_instance_key: binding.canonical_source_instance_key(),
            stream_key,
            object_key,
        },
    })
}

impl ScopedObservationAccessPass {
    pub(crate) fn reserve_observation_runtime_source<'pass>(
        &'pass self,
        request: ScopeAccessRequest<'_>,
    ) -> Result<ScopedObservationRuntimeSourceReservation<'pass>, ScopedObservationRuntimeSourceError>
    {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationRuntimeSourceError::Closed);
        }
        let runtime = self
            .plan
            .reserve_observation_source(request)?
            .bind_runtime_stream(self.adapter.as_ref(), self.source_instance.as_ref())?;
        let Some(approved_root) = self.access_roots.get(runtime.access_root()) else {
            runtime.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        };
        let binding = ScopedObservationRuntimeSourceBinding::bind(
            runtime,
            self.source_instance.as_ref(),
            approved_root,
            &self.root_identity.source_instance_key,
        )?;
        if self.state.closed.load(Ordering::Acquire) {
            binding.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::Closed);
        }
        Ok(ScopedObservationRuntimeSourceReservation {
            _pass: self,
            binding,
        })
    }
}

#[cfg(test)]
pub(crate) fn bind_observation_runtime_source_for_test(
    runtime: AuthorizedObservationRuntimeStreamReservation,
    instance: &SourceInstance,
    approved_root: &ScopedAccessRootGrant,
    expected_source_instance_key: &CanonicalSourceInstanceKey,
) -> Result<ScopedObservationRuntimeSourceBinding, ScopedObservationRuntimeSourceError> {
    ScopedObservationRuntimeSourceBinding::bind(
        runtime,
        instance,
        approved_root,
        expected_source_instance_key,
    )
}

#[cfg(test)]
pub(crate) fn prepare_observation_directory_membership_for_test(
    binding: &ScopedObservationRuntimeSourceBinding,
) -> Result<ScopedObservationDirectoryMembershipProof, ScopedObservationRuntimeSourceError> {
    prepare_directory_membership_contract(binding)
}
