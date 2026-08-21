//! Attachment-owned binding for authorized dynamic observation sources.
//!
//! This module still performs no native I/O. It joins the declaration/runtime
//! stream proof to the exact source-instance root already approved by the
//! scoped attachment, while retaining the access reservation and borrowing the
//! pass that owns it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::adapter::{CanonicalSourceInstanceKey, SourceInstance, SourceInstanceKey, StreamSpec};
use crate::source::{
    AccessBudgetError, AccessObjectToken, AccessOutcome,
    AuthorizedObservationRuntimeStreamReservation, ScopeAccessRequest,
};

use super::{ScopedAccessRootGrant, ScopedObservationAccessPass};

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

impl fmt::Debug for ScopedObservationRuntimeSourceReservation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.binding.fmt(formatter)
    }
}

impl ScopedObservationRuntimeSourceReservation<'_> {
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
}

impl ScopedObservationAccessPass {
    pub(crate) fn reserve_observation_runtime_source<'pass>(
        &'pass self,
        instance: &SourceInstance,
        request: ScopeAccessRequest<'_>,
    ) -> Result<ScopedObservationRuntimeSourceReservation<'pass>, ScopedObservationRuntimeSourceError>
    {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationRuntimeSourceError::Closed);
        }
        let runtime = self
            .plan
            .reserve_observation_source(request)?
            .bind_runtime_stream(self.adapter.as_ref(), instance)?;
        let Some(approved_root) = self.access_roots.get(runtime.access_root()) else {
            runtime.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        };
        let binding = ScopedObservationRuntimeSourceBinding::bind(
            runtime,
            instance,
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
