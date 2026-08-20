//! Confined artifact relation authorization and capture.
//!
//! This seam consumes current admitted artifact metadata only far enough to
//! reserve one exact promoted `ArtifactLocatorFromEvidence` relation and
//! render its declaration template from already-bound identity evidence. A
//! consumed reservation may then use the common no-follow stable-file driver
//! to produce a bounded private capture. An attachment-owned generation ledger
//! binds that capture to common ReplaceDocument-style presence lineage before
//! the existing strict wire contract may serialize an outcome. A changed
//! availability occurrence enters the attachment's ordered semantic lane
//! before its reducer mutation commits, so backpressure advances neither.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::adapter::{
    ContractCompleteness, CoverageObjectKey, CoverageStreamKey, QualifiedValueQuality,
    ScopeRelationPrimitive, Sha256Digest,
};
use crate::source::{
    confined_relative_path_key, read_stable_file_confined, validate_evidence_locator_template,
    AccessBudgetError, AccessObjectToken, AccessOperation, AccessOutcome, AccessPhase,
    AuthorizedScopeAccessPlan, FileIdentity, Revision, ScopeAccessRequest, ScopeAccessReservation,
    ScopeIdentityInput, StableRead,
};

use super::artifact_availability::{
    ScopedArtifactAvailabilityObservation, ScopedArtifactAvailabilitySourceOccurrence,
    ScopedArtifactAvailabilityState,
};
use super::artifact_evidence::MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS;
use super::artifact_wire::{
    ScopedArtifactContractError, ScopedArtifactReadOutcome, ScopedArtifactUnavailableReason,
    ScopedObservedArtifactWire, ScopedValidatedArtifactReadCommand,
};
use super::{
    artifact_availability_event_id, source_belongs_to_root, ScopedAccessRootGrant,
    ScopedAppendDeliveryPhase, ScopedArtifactContentPolicy, ScopedArtifactRelationGrant,
    ScopedDeliveryError, ScopedKnownObjectGrant, ScopedObservationAccessError,
    ScopedObservationAccessPass, ScopedObservationConsumerDrain, ScopedObservationContinuity,
    ScopedObservationOfferReceipt, ScopedProjectedObservation, ScopedSourceFailureClass,
    ScopedSourceObjectIdentity,
};

const ARTIFACT_IDENTITY_INPUTS: [&str; 3] =
    ["native-session-id", "backup-name", "artifact-version"];

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScopedArtifactRelationAccessError {
    #[error("artifact relation proof does not match the active attachment and pass")]
    InvalidBinding,
    #[error("artifact relation requires an exact complete native session identity")]
    NativeSessionUnavailable,
    #[error("scoped artifact access closed before the confined read completed")]
    Closed,
    #[error("scoped artifact confined read failed: {0:?}")]
    Source(ScopedSourceFailureClass),
    #[error("scoped artifact generation ledger is full")]
    GenerationCapacityExhausted,
    #[error("scoped artifact source generation overflowed")]
    GenerationExhausted,
    #[error(transparent)]
    Access(#[from] AccessBudgetError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedArtifactObservationOfferError {
    #[error("scoped artifact capture was already offered")]
    AlreadyOffered,
    #[error("scoped artifact consumer drain belongs to another attachment")]
    ForeignDrain,
    #[error("scoped artifact attachment or consumer drain is closed")]
    Closed,
    #[error("scoped artifact capture does not belong to the current delivery epoch")]
    InvalidEpoch,
    #[error("scoped artifact capture lost its exact authorized source binding")]
    InvalidSourceBinding,
    #[error("scoped artifact offer retry changed its frozen observation time")]
    ObservationTimeDrift,
    #[error("scoped artifact availability reducer is full or rejected the occurrence")]
    AvailabilityRejected,
    #[error(transparent)]
    Contract(#[from] ScopedArtifactContractError),
    #[error("scoped artifact availability event could not enter delivery: {0}")]
    Delivery(ScopedDeliveryError),
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
    source_binding: ScopedArtifactSourceObjectBinding,
    _root: PathBuf,
    _relative_path: PathBuf,
    _native_session_id: Arc<str>,
    _native_artifact_id: Arc<str>,
    _artifact_version: Arc<str>,
}

/// Exact path-free source coordinate derived from the promoted scope
/// relation's checked source binding and the confined rendered locator. It is
/// retained privately for the future ordered availability offer; possession
/// does not assign an observer sequence or authorize another read.
#[derive(Clone, PartialEq, Eq)]
struct ScopedArtifactSourceObjectBinding {
    source_declaration_digest: [u8; 32],
    source: ScopedSourceObjectIdentity,
    max_object_bytes: u64,
}

impl fmt::Debug for ScopedArtifactSourceObjectBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedArtifactSourceObjectBinding")
            .field("adapter_id", &self.source.adapter_id)
            .field("max_object_bytes", &self.max_object_bytes)
            .field("source_declaration_digest", &"sha256:<redacted>")
            .field("source_instance", &"<redacted>")
            .field("stream", &"<redacted>")
            .field("object", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScopedArtifactGenerationEntry {
    generation: u64,
    present: bool,
}

/// Attachment-local source generation lineage for evidence-derived artifact
/// objects. This mirrors the common ReplaceDocument presence semantics: a
/// content revision or native file-identity replacement is same-generation,
/// while an observed delete or recreation advances the generation. The exact
/// relation/session/backup/version object token prevents cross-object reuse.
pub(super) struct ScopedArtifactGenerationLedger {
    objects: BTreeMap<AccessObjectToken, ScopedArtifactGenerationEntry>,
}

impl ScopedArtifactGenerationLedger {
    pub(super) fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
        }
    }

    fn preflight(
        &self,
        object_token: AccessObjectToken,
    ) -> Result<(), ScopedArtifactRelationAccessError> {
        if !self.objects.contains_key(&object_token)
            && self.objects.len() >= MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS
        {
            return Err(ScopedArtifactRelationAccessError::GenerationCapacityExhausted);
        }
        Ok(())
    }

    fn observe(
        &mut self,
        object_token: AccessObjectToken,
        present: bool,
    ) -> Result<Option<u64>, ScopedArtifactRelationAccessError> {
        let Some(previous) = self.objects.get(&object_token).copied() else {
            if !present {
                return Ok(None);
            }
            self.objects.insert(
                object_token,
                ScopedArtifactGenerationEntry {
                    generation: 1,
                    present: true,
                },
            );
            return Ok(Some(1));
        };
        let generation = if previous.present == present {
            previous.generation
        } else {
            previous
                .generation
                .checked_add(1)
                .ok_or(ScopedArtifactRelationAccessError::GenerationExhausted)?
        };
        self.objects.insert(
            object_token,
            ScopedArtifactGenerationEntry {
                generation,
                present,
            },
        );
        Ok(Some(generation))
    }

    fn current_or_initial_generation(&self, object_token: AccessObjectToken) -> u64 {
        self.objects
            .get(&object_token)
            .map_or(1, |entry| entry.generation)
    }
}

enum ScopedArtifactConfinedCaptureState {
    Missing {
        observed_generation: Option<u64>,
        provenance_ref: Option<[u8; 32]>,
    },
    Oversized {
        observed_bytes: u64,
        generation: u64,
        provenance_ref: [u8; 32],
    },
    Unstable {
        source_generation: u64,
    },
    Stable {
        _file_identity: FileIdentity,
        _modified_ns: i128,
        _revision: Revision,
        generation: u64,
        provenance_ref: [u8; 32],
        size_bytes: u64,
        content_hash: Option<Sha256Digest>,
        content: Option<Vec<u8>>,
    },
}

/// One bounded native capture with an attachment-local source generation. The
/// borrowed validated command remains attached through generation comparison
/// and portable outcome construction. Native identity, revision, hash, and
/// bytes are redacted from Debug; disclosure into the wire remains governed by
/// the exact validated content policy.
pub(crate) struct ScopedArtifactConfinedCapture<'command, 'pass> {
    _validated: ScopedValidatedArtifactReadCommand<'command>,
    _pass: &'pass ScopedObservationAccessPass,
    object_token: AccessObjectToken,
    source_binding: ScopedArtifactSourceObjectBinding,
    state: ScopedArtifactConfinedCaptureState,
    offer_observed_at: Option<i64>,
    offered: bool,
}

/// Unforgeable zero-sized witness that portable outcome construction consumed
/// one generation-bound confined capture. Its tuple constructor is private to
/// this module, so evidence validation alone cannot mint an observed result.
pub(super) struct ScopedArtifactOutcomeAuthority(());

impl fmt::Debug for ScopedArtifactConfinedCapture<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (
            state,
            observed_bytes,
            has_generation,
            has_provenance,
            has_content_hash,
            has_inline_content,
        ) = match &self.state {
            ScopedArtifactConfinedCaptureState::Missing {
                observed_generation,
                provenance_ref,
            } => (
                "missing",
                None,
                observed_generation.is_some(),
                provenance_ref.is_some(),
                false,
                false,
            ),
            ScopedArtifactConfinedCaptureState::Oversized { observed_bytes, .. } => {
                ("oversized", Some(*observed_bytes), true, true, false, false)
            }
            ScopedArtifactConfinedCaptureState::Unstable { .. } => {
                ("unstable", None, true, false, false, false)
            }
            ScopedArtifactConfinedCaptureState::Stable {
                size_bytes,
                content_hash,
                content,
                ..
            } => (
                "stable",
                Some(*size_bytes),
                true,
                true,
                content_hash.is_some(),
                content.is_some(),
            ),
        };
        formatter
            .debug_struct("ScopedArtifactConfinedCapture")
            .field("object_token", &self.object_token)
            .field("source_binding", &self.source_binding)
            .field("state", &state)
            .field("observed_bytes", &observed_bytes)
            .field("has_generation", &has_generation)
            .field("has_provenance", &has_provenance)
            .field("has_content_hash", &has_content_hash)
            .field("has_inline_content", &has_inline_content)
            .field("native_path", &"<redacted>")
            .field("native_identity", &"<redacted>")
            .field("native_revision", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ScopedArtifactConfinedCapture<'_, '_> {
    /// Atomically offer one changed availability occurrence before committing
    /// its reducer mutation. Backpressure consumes neither observer sequence
    /// nor availability state, so this same bounded capture can be retried.
    pub(crate) fn offer_observed(
        &mut self,
        drain: &mut ScopedObservationConsumerDrain,
        observed_at: i64,
    ) -> Result<
        (
            ScopedObservedArtifactWire,
            Option<ScopedObservationOfferReceipt>,
        ),
        ScopedArtifactObservationOfferError,
    > {
        if self.offered {
            return Err(ScopedArtifactObservationOfferError::AlreadyOffered);
        }
        if !Arc::ptr_eq(
            &self._pass.attachment_authority,
            &drain.attachment_authority,
        ) {
            return Err(ScopedArtifactObservationOfferError::ForeignDrain);
        }
        if drain.is_closed()
            || self
                ._pass
                .state
                .closed
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ScopedArtifactObservationOfferError::Closed);
        }
        let delivery_state = drain.delivery_lane().state();
        let phase = match delivery_state.continuity {
            ScopedObservationContinuity::Bootstrap => ScopedAppendDeliveryPhase::Bootstrap,
            ScopedObservationContinuity::Valid => ScopedAppendDeliveryPhase::Live,
            ScopedObservationContinuity::ResyncRequired
            | ScopedObservationContinuity::Resyncing
            | ScopedObservationContinuity::Failed => {
                return Err(ScopedArtifactObservationOfferError::InvalidEpoch);
            }
        };
        if self._validated.scope_epoch() != delivery_state.scope_epoch {
            return Err(ScopedArtifactObservationOfferError::InvalidEpoch);
        }
        if self.source_binding.source_declaration_digest
            != *self._pass.plan.source_declaration_digest()
            || !source_belongs_to_root(&self.source_binding.source, &self._pass.root_identity)
            || self._validated.command.max_bytes > self.source_binding.max_object_bytes
        {
            return Err(ScopedArtifactObservationOfferError::InvalidSourceBinding);
        }

        let observed = self._validated.observed(
            ScopedArtifactOutcomeAuthority(()),
            artifact_read_outcome(&self.state, self._validated.expected_generation()),
        )?;
        match self.offer_observed_at {
            Some(expected) if expected != observed_at => {
                return Err(ScopedArtifactObservationOfferError::ObservationTimeDrift);
            }
            Some(_) => {}
            None => self.offer_observed_at = Some(observed_at),
        }
        let observation = artifact_availability_observation(
            &self._validated,
            self.object_token,
            &self.source_binding,
            &self.state,
        );
        let mut availability = self
            ._pass
            .state
            .artifact_availability
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self
            ._pass
            .state
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
            || drain.is_closed()
        {
            return Err(ScopedArtifactObservationOfferError::Closed);
        }
        let Some(prepared) = availability
            .prepare_observe(self._validated.command.root.session_key, observation)
            .map_err(|()| ScopedArtifactObservationOfferError::AvailabilityRejected)?
        else {
            self.offered = true;
            return Ok((observed, None));
        };
        let occurrence = prepared.occurrence().clone();
        let event_id = artifact_availability_event_id(&occurrence);
        let receipt = drain
            .delivery_lane_mut()
            .offer_projected(vec![ScopedProjectedObservation::ArtifactAvailability {
                observed_at,
                phase,
                event_id,
                occurrence: Box::new(occurrence),
            }])
            .map_err(|failure| ScopedArtifactObservationOfferError::Delivery(failure.error))?;
        availability.commit_observe(prepared);
        self.offered = true;
        Ok((observed, Some(receipt)))
    }

    /// Low-level fixture helper for the already-frozen artifact result wire.
    /// It deliberately performs no availability mutation or ordered offer.
    #[cfg(test)]
    pub(crate) fn into_observed_for_test(
        self,
    ) -> Result<ScopedObservedArtifactWire, ScopedArtifactContractError> {
        if self
            ._pass
            .state
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ScopedArtifactContractError::Closed);
        }
        self._validated.observed(
            ScopedArtifactOutcomeAuthority(()),
            artifact_read_outcome(&self.state, self._validated.expected_generation()),
        )
    }
}

fn artifact_availability_observation(
    validated: &ScopedValidatedArtifactReadCommand<'_>,
    object_token: AccessObjectToken,
    source_binding: &ScopedArtifactSourceObjectBinding,
    state: &ScopedArtifactConfinedCaptureState,
) -> ScopedArtifactAvailabilityObservation {
    let native_state = match state {
        ScopedArtifactConfinedCaptureState::Missing {
            observed_generation,
            provenance_ref,
        } => ScopedArtifactAvailabilityState::Missing {
            observed_generation: *observed_generation,
            provenance_ref: *provenance_ref,
        },
        ScopedArtifactConfinedCaptureState::Oversized {
            observed_bytes,
            generation,
            provenance_ref,
        } => ScopedArtifactAvailabilityState::OverLimit {
            generation: *generation,
            provenance_ref: *provenance_ref,
            observed_bytes: *observed_bytes,
            request_max_bytes: validated.command.max_bytes,
        },
        ScopedArtifactConfinedCaptureState::Unstable { .. } => {
            ScopedArtifactAvailabilityState::Unstable
        }
        ScopedArtifactConfinedCaptureState::Stable {
            generation,
            provenance_ref,
            size_bytes,
            ..
        } => ScopedArtifactAvailabilityState::Available {
            generation: *generation,
            provenance_ref: *provenance_ref,
            size_bytes: *size_bytes,
        },
    };
    ScopedArtifactAvailabilityObservation::new(
        validated
            .command
            .artifact_evidence
            .as_ref()
            .expect("a validated artifact capture retains exact metadata evidence")
            .clone(),
        Arc::from(validated.command.artifact_kind.as_str()),
        Arc::clone(
            validated
                .command
                .artifact_relation_id
                .as_ref()
                .expect("a validated artifact capture retains an exact relation"),
        ),
        object_token,
        ScopedArtifactAvailabilitySourceOccurrence::new(
            source_binding.source_declaration_digest,
            source_binding.source.clone(),
            source_generation(state),
        ),
        native_state,
    )
}

fn source_generation(state: &ScopedArtifactConfinedCaptureState) -> u64 {
    match state {
        ScopedArtifactConfinedCaptureState::Missing {
            observed_generation,
            ..
        } => observed_generation.unwrap_or(1),
        ScopedArtifactConfinedCaptureState::Oversized { generation, .. }
        | ScopedArtifactConfinedCaptureState::Stable { generation, .. } => *generation,
        ScopedArtifactConfinedCaptureState::Unstable { source_generation } => *source_generation,
    }
}

fn artifact_read_outcome(
    state: &ScopedArtifactConfinedCaptureState,
    expected_generation: Option<u64>,
) -> ScopedArtifactReadOutcome {
    match state {
        ScopedArtifactConfinedCaptureState::Missing {
            observed_generation,
            provenance_ref,
        } => ScopedArtifactReadOutcome::Unavailable {
            reason: if generation_changed(expected_generation, *observed_generation) {
                ScopedArtifactUnavailableReason::ChangedGeneration
            } else {
                ScopedArtifactUnavailableReason::Missing
            },
            observed_generation: *observed_generation,
            observed_bytes: None,
            provenance_ref: *provenance_ref,
        },
        ScopedArtifactConfinedCaptureState::Oversized {
            observed_bytes,
            generation,
            provenance_ref,
        } => {
            let changed = generation_changed(expected_generation, Some(*generation));
            ScopedArtifactReadOutcome::Unavailable {
                reason: if changed {
                    ScopedArtifactUnavailableReason::ChangedGeneration
                } else {
                    ScopedArtifactUnavailableReason::OverLimit
                },
                observed_generation: Some(*generation),
                observed_bytes: (!changed).then_some(*observed_bytes),
                provenance_ref: Some(*provenance_ref),
            }
        }
        ScopedArtifactConfinedCaptureState::Unstable { .. } => {
            ScopedArtifactReadOutcome::Unavailable {
                reason: ScopedArtifactUnavailableReason::Unstable,
                observed_generation: None,
                observed_bytes: None,
                provenance_ref: None,
            }
        }
        ScopedArtifactConfinedCaptureState::Stable {
            generation,
            provenance_ref,
            ..
        } if generation_changed(expected_generation, Some(*generation)) => {
            ScopedArtifactReadOutcome::Unavailable {
                reason: ScopedArtifactUnavailableReason::ChangedGeneration,
                observed_generation: Some(*generation),
                observed_bytes: None,
                provenance_ref: Some(*provenance_ref),
            }
        }
        ScopedArtifactConfinedCaptureState::Stable {
            generation,
            provenance_ref,
            size_bytes,
            content_hash,
            content,
            ..
        } => ScopedArtifactReadOutcome::Available {
            generation: *generation,
            provenance_ref: *provenance_ref,
            size_bytes: *size_bytes,
            content_hash: *content_hash,
            content: content.clone(),
        },
    }
}

fn generation_changed(expected: Option<u64>, observed: Option<u64>) -> bool {
    expected
        .zip(observed)
        .is_some_and(|(expected, observed)| expected != observed)
}

impl<'command, 'pass> ScopedArtifactRelationReservation<'command, 'pass> {
    /// Consume this exact reservation with the common stable-file driver and
    /// the attachment's bounded source-generation ledger. Portable outcome
    /// construction remains a separate consuming step so generation mismatch
    /// can discard retained content before serialization.
    pub(crate) fn read_confined(
        self,
    ) -> Result<ScopedArtifactConfinedCapture<'command, 'pass>, ScopedArtifactRelationAccessError>
    {
        let Self {
            _validated: validated,
            _pass: pass,
            _reservation: reservation,
            object_token,
            max_bytes,
            source_binding,
            _root: root,
            _relative_path: relative_path,
            ..
        } = self;
        if pass.state.closed.load(std::sync::atomic::Ordering::Acquire) {
            reservation.complete(0, 0, AccessOutcome::Unavailable)?;
            return Err(ScopedArtifactRelationAccessError::Closed);
        }
        let max_bytes = match usize::try_from(max_bytes) {
            Ok(max_bytes) => max_bytes,
            Err(_) => {
                reservation.fail_conservative();
                return Err(ScopedArtifactRelationAccessError::Access(
                    AccessBudgetError::InvalidConfig(
                        "artifact byte reservation exceeds this platform".to_string(),
                    ),
                ));
            }
        };
        let mut generations = pass
            .state
            .artifact_generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(error) = generations.preflight(object_token) {
            reservation.complete(0, 0, AccessOutcome::Failed)?;
            return Err(error);
        }
        let read = match read_stable_file_confined(&root, &relative_path, max_bytes) {
            Ok(read) => read,
            Err(error) => {
                reservation.fail_conservative();
                return Err(ScopedArtifactRelationAccessError::Source(
                    super::source_failure_class(&error),
                ));
            }
        };
        if pass.state.closed.load(std::sync::atomic::Ordering::Acquire) {
            reservation.fail_conservative();
            return Err(ScopedArtifactRelationAccessError::Closed);
        }
        let content_policy = validated.content_policy();
        let evidence_revision = validated.evidence_revision();
        let state = match read {
            StableRead::Missing => {
                let observed_generation = match generations.observe(object_token, false) {
                    Ok(generation) => generation,
                    Err(error) => {
                        reservation.complete(0, 0, AccessOutcome::Failed)?;
                        return Err(error);
                    }
                };
                let provenance_ref = observed_generation.map(|generation| {
                    artifact_provenance_ref(
                        object_token,
                        generation,
                        evidence_revision,
                        ArtifactProvenanceObservation::Missing,
                    )
                });
                reservation.complete(0, 0, AccessOutcome::Unavailable)?;
                ScopedArtifactConfinedCaptureState::Missing {
                    observed_generation,
                    provenance_ref,
                }
            }
            StableRead::Oversized(stamp) => {
                let generation = match generations.observe(object_token, true) {
                    Ok(Some(generation)) => generation,
                    Ok(None) => unreachable!("present observations always establish generation"),
                    Err(error) => {
                        reservation.complete(0, 0, AccessOutcome::Failed)?;
                        return Err(error);
                    }
                };
                let provenance_ref = artifact_provenance_ref(
                    object_token,
                    generation,
                    evidence_revision,
                    ArtifactProvenanceObservation::FileStamp {
                        identity: &stamp.identity,
                        modified_ns: stamp.modified_ns,
                        size_bytes: stamp.len,
                        revision: None,
                    },
                );
                reservation.complete(0, 0, AccessOutcome::Oversized)?;
                ScopedArtifactConfinedCaptureState::Oversized {
                    observed_bytes: stamp.len,
                    generation,
                    provenance_ref,
                }
            }
            StableRead::Unstable => {
                let source_generation = generations.current_or_initial_generation(object_token);
                reservation.fail_conservative();
                ScopedArtifactConfinedCaptureState::Unstable { source_generation }
            }
            StableRead::Stable {
                stamp,
                bytes,
                revision,
            } => {
                let size_bytes = bytes.len() as u64;
                let generation = match generations.observe(object_token, true) {
                    Ok(Some(generation)) => generation,
                    Ok(None) => unreachable!("present observations always establish generation"),
                    Err(error) => {
                        reservation.complete(size_bytes, 0, AccessOutcome::Failed)?;
                        return Err(error);
                    }
                };
                let provenance_ref = artifact_provenance_ref(
                    object_token,
                    generation,
                    evidence_revision,
                    ArtifactProvenanceObservation::FileStamp {
                        identity: &stamp.identity,
                        modified_ns: stamp.modified_ns,
                        size_bytes,
                        revision: Some(revision),
                    },
                );
                let content_hash = matches!(
                    content_policy,
                    ScopedArtifactContentPolicy::HashOnly | ScopedArtifactContentPolicy::Inline
                )
                .then(|| Sha256Digest::of(&bytes));
                let content =
                    (content_policy == ScopedArtifactContentPolicy::Inline).then_some(bytes);
                reservation.complete(size_bytes, 0, AccessOutcome::Available)?;
                ScopedArtifactConfinedCaptureState::Stable {
                    _file_identity: stamp.identity,
                    _modified_ns: stamp.modified_ns,
                    _revision: revision,
                    generation,
                    provenance_ref,
                    size_bytes,
                    content_hash,
                    content,
                }
            }
        };
        Ok(ScopedArtifactConfinedCapture {
            _validated: validated,
            _pass: pass,
            object_token,
            source_binding,
            state,
            offer_observed_at: None,
            offered: false,
        })
    }
}

enum ArtifactProvenanceObservation<'a> {
    Missing,
    FileStamp {
        identity: &'a FileIdentity,
        modified_ns: i128,
        size_bytes: u64,
        revision: Option<Revision>,
    },
}

fn artifact_provenance_ref(
    object_token: AccessObjectToken,
    generation: u64,
    evidence_revision: [u8; 32],
    observation: ArtifactProvenanceObservation<'_>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/artifact-provenance/v1\0");
    hasher.update(object_token.as_bytes());
    hasher.update(&generation.to_be_bytes());
    hasher.update(&evidence_revision);
    match observation {
        ArtifactProvenanceObservation::Missing => {
            hasher.update(&[1]);
        }
        ArtifactProvenanceObservation::FileStamp {
            identity,
            modified_ns,
            size_bytes,
            revision,
        } => {
            hasher.update(&[2]);
            let mut identity_bytes = Vec::with_capacity(32);
            identity.encode_into(&mut identity_bytes);
            hasher.update(&(identity_bytes.len() as u64).to_be_bytes());
            hasher.update(&identity_bytes);
            hasher.update(&modified_ns.to_be_bytes());
            hasher.update(&size_bytes.to_be_bytes());
            match revision {
                Some(revision) => {
                    hasher.update(&[1]);
                    hasher.update(revision.as_bytes());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
    };
    *hasher.finalize().as_bytes()
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
            || relation.source_binding.is_none()
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
    let mut expected = plan
        .observation_relations()
        .map(|relation| relation.access_root.as_str())
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
        let declared_source = relation
            .source_binding
            .as_ref()
            .ok_or(ScopedArtifactRelationAccessError::InvalidBinding)?;
        if validated.command.max_bytes <= relation.bounds.max_bytes
            && validated.command.max_bytes > declared_source.max_object_bytes
        {
            return Err(ScopedArtifactRelationAccessError::InvalidBinding);
        }
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
        let source_object_key = match confined_relative_path_key(&relative_path) {
            Ok(value) => value,
            Err(_) => {
                reservation.fail_conservative();
                return Err(ScopedArtifactRelationAccessError::InvalidBinding);
            }
        };
        let stream_key = match CoverageStreamKey::derive(
            self.root_identity.adapter_id.as_str(),
            declared_source.stream_id.as_bytes(),
        ) {
            Ok(stream_key) => stream_key,
            Err(_) => {
                reservation.fail_conservative();
                return Err(ScopedArtifactRelationAccessError::InvalidBinding);
            }
        };
        let object_key =
            match CoverageObjectKey::derive(&declared_source.stream_id, &source_object_key) {
                Ok(object_key) => object_key,
                Err(_) => {
                    reservation.fail_conservative();
                    return Err(ScopedArtifactRelationAccessError::InvalidBinding);
                }
            };
        let source_binding = ScopedArtifactSourceObjectBinding {
            source_declaration_digest: *self.plan.source_declaration_digest(),
            source: ScopedSourceObjectIdentity {
                adapter_id: self.root_identity.adapter_id.clone(),
                source_instance_key: self.root_identity.source_instance_key,
                stream_key,
                object_key,
            },
            max_object_bytes: declared_source.max_object_bytes,
        };
        let object_token = reservation.object_token();
        Ok(ScopedArtifactRelationReservation {
            relation_id: Arc::from(relation_id),
            access_root: Arc::from(reservation.access_root()),
            locator_id: Arc::from(reservation.locator()),
            artifact_kind: Arc::from(validated.command.artifact_kind.as_str()),
            object_token,
            max_bytes: validated.command.max_bytes,
            source_binding,
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
