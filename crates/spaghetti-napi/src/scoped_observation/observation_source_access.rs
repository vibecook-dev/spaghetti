//! Attachment-owned binding for authorized dynamic observation sources.
//!
//! This module still performs no native I/O. It joins the declaration/runtime
//! stream proof to the exact source-instance root already approved by the
//! scoped attachment, while retaining the access reservation and borrowing the
//! pass that owns it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::adapter::{
    AdapterId, CanonicalSourceInstanceKey, CoverageObjectKey, CoverageStreamKey,
    ScopeRelationPrimitive, SourceInstance, SourceInstanceKey, StreamSpec,
};
use crate::source::{
    confined_relative_path_key, AccessBudgetError, AccessObjectToken, AccessOperation,
    AccessOutcome, AuthorizedObservationDirectoryEntryReservation,
    AuthorizedObservationDirectoryRootAuthority, AuthorizedObservationRuntimeStreamReservation,
    DirectoryEntryAuditReservation, DirectoryEntryAuditor, DirectoryEntryKind, DirectorySelection,
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
    authority: AuthorizedObservationDirectoryRootAuthority,
    root_object_token: AccessObjectToken,
    // Every yielded entry is retained only as an opaque token plus kind,
    // including entries the declaration selector does not retain.
    entries: BTreeMap<AccessObjectToken, DirectoryEntryKind>,
    failed: bool,
}

/// Completed, path-free evidence that one exact native directory entry was
/// accounted beneath the listing root before it can enter membership.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScopedObservationDirectoryEntry {
    object_token: AccessObjectToken,
    kind: DirectoryEntryKind,
    depth: u32,
}

pub(crate) struct ScopedObservationDirectoryEntryReservation<'audit> {
    proof: &'audit mut ScopedObservationDirectoryMembershipProof,
    reservation: Option<AuthorizedObservationDirectoryEntryReservation>,
    object_token: AccessObjectToken,
    depth: u32,
    file_selected: bool,
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
            .field("accounted_entries", &self.entries.len())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryEntry")
            .field("kind", &self.kind)
            .field("depth", &self.depth)
            .field("has_object_token", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryEntryReservation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryEntryReservation")
            .field("depth", &self.depth)
            .field("has_object_token", &true)
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

    pub(crate) fn reserve_entry(
        &mut self,
        relative_path: &Path,
    ) -> Result<ScopedObservationDirectoryEntryReservation<'_>, ScopedObservationRuntimeSourceError>
    {
        self.proof
            .reserve_entry(&self.reservation.binding, relative_path)
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

    pub(crate) fn accounted_entries(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn select(
        &self,
        relative_path: &Path,
        kind: DirectoryEntryKind,
    ) -> DirectorySelection {
        if !GlobPattern::accepts_relative_path(relative_path) {
            return DirectorySelection::Ignore;
        }
        match kind {
            DirectoryEntryKind::Directory => DirectorySelection::Recurse,
            DirectoryEntryKind::File if self.selector.matches_path(relative_path) => {
                DirectorySelection::Include
            }
            DirectoryEntryKind::File => DirectorySelection::Ignore,
        }
    }

    pub(crate) fn reserve_entry<'audit>(
        &'audit mut self,
        binding: &ScopedObservationRuntimeSourceBinding,
        relative_path: &Path,
    ) -> Result<
        ScopedObservationDirectoryEntryReservation<'audit>,
        ScopedObservationRuntimeSourceError,
    > {
        if self.failed || !GlobPattern::accepts_relative_path(relative_path) {
            self.failed = true;
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        let relative_path_key = confined_relative_path_key(relative_path)
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let file_selected = self.selector.matches_path(relative_path);
        let parent = relative_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        let parent_path_key = parent
            .map(confined_relative_path_key)
            .transpose()
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let component_count = relative_path
            .components()
            .filter(|component| matches!(component, std::path::Component::Normal(_)))
            .count();
        let depth = u32::try_from(component_count)
            .ok()
            .filter(|depth| *depth > 0)
            .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let parent_token = match parent_path_key.as_deref() {
            Some(parent_key) => {
                let token = binding
                    .runtime
                    .directory_entry_token(&self.authority, parent_key)
                    .map_err(ScopedObservationRuntimeSourceError::Access)?;
                if self.entries.get(&token) != Some(&DirectoryEntryKind::Directory) {
                    self.failed = true;
                    return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
                }
                token
            }
            None => self.root_object_token,
        };
        let reservation = match binding.runtime.reserve_directory_entry(
            &self.authority,
            &relative_path_key,
            parent_path_key.as_deref(),
            depth,
        ) {
            Ok(reservation) => reservation,
            Err(error) => {
                self.failed = true;
                return Err(ScopedObservationRuntimeSourceError::Access(error));
            }
        };
        let object_token = reservation.object_token();
        if object_token == parent_token || self.entries.contains_key(&object_token) {
            reservation.fail_conservative();
            self.failed = true;
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        Ok(ScopedObservationDirectoryEntryReservation {
            proof: self,
            reservation: Some(reservation),
            object_token,
            depth,
            file_selected,
        })
    }
}

impl ScopedObservationDirectoryEntry {
    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.object_token
    }

    pub(crate) fn kind(&self) -> DirectoryEntryKind {
        self.kind
    }

    pub(crate) fn depth(&self) -> u32 {
        self.depth
    }
}

impl ScopedObservationDirectoryEntryReservation<'_> {
    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.object_token
    }

    pub(crate) fn selection(&self, kind: DirectoryEntryKind) -> DirectorySelection {
        match kind {
            DirectoryEntryKind::Directory => DirectorySelection::Recurse,
            DirectoryEntryKind::File if self.file_selected => DirectorySelection::Include,
            DirectoryEntryKind::File => DirectorySelection::Ignore,
        }
    }

    pub(crate) fn complete(
        mut self,
        kind: DirectoryEntryKind,
    ) -> Result<ScopedObservationDirectoryEntry, ScopedObservationRuntimeSourceError> {
        let reservation = self
            .reservation
            .take()
            .expect("directory entry reservation is consumed only once");
        if let Err(error) = reservation.complete() {
            self.proof.failed = true;
            return Err(ScopedObservationRuntimeSourceError::Access(error));
        }
        if self.proof.entries.insert(self.object_token, kind).is_some() {
            self.proof.failed = true;
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        Ok(ScopedObservationDirectoryEntry {
            object_token: self.object_token,
            kind,
            depth: self.depth,
        })
    }
}

impl DirectoryEntryAuditor for ScopedObservationDirectoryMembershipContract<'_> {
    type Error = ScopedObservationRuntimeSourceError;
    type Reservation<'audit>
        = ScopedObservationDirectoryEntryReservation<'audit>
    where
        Self: 'audit;

    fn reserve_entry<'audit>(
        &'audit mut self,
        relative_path: &Path,
    ) -> Result<Self::Reservation<'audit>, Self::Error> {
        ScopedObservationDirectoryMembershipContract::reserve_entry(self, relative_path)
    }
}

impl DirectoryEntryAuditReservation for ScopedObservationDirectoryEntryReservation<'_> {
    type Error = ScopedObservationRuntimeSourceError;

    fn selection(&self, kind: DirectoryEntryKind) -> DirectorySelection {
        ScopedObservationDirectoryEntryReservation::selection(self, kind)
    }

    fn complete(self, kind: DirectoryEntryKind) -> Result<(), Self::Error> {
        ScopedObservationDirectoryEntryReservation::complete(self, kind).map(|_| ())
    }
}

impl Drop for ScopedObservationDirectoryEntryReservation<'_> {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        reservation.fail_conservative();
        self.proof.failed = true;
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
    // The listing root is already one reserved object at depth one. The
    // confined driver therefore receives only the remaining child-object
    // capacity and one fewer relative recursion level. Per-directory fan-out
    // is additionally capped by that remaining aggregate capacity.
    let child_capacity = bounds
        .max_objects
        .checked_sub(1)
        .filter(|capacity| *capacity > 0)
        .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let config = DirectorySnapshotConfig {
        max_entries: usize::try_from(child_capacity)
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
        max_entries_per_directory: usize::try_from(bounds.max_fan_out.min(child_capacity))
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
        max_depth: usize::try_from(bounds.max_depth.saturating_sub(1))
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
        authority: binding.runtime.directory_root_authority()?,
        root_object_token: binding.object_token(),
        entries: BTreeMap::new(),
        failed: false,
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
