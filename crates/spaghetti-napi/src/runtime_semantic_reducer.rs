//! Topology-neutral RFC 012C revisioned-entity reduction primitives.
//!
//! Durable ingestion and scoped observation carry different delivery state,
//! but they must make the same semantic decision and publish the same reduced
//! state digest at an equal source/family coverage vector.  This module owns
//! that shared law; it deliberately contains no database or observer types.

use crate::adapter::{
    AdapterId, CanonicalSourceInstanceKey, ContractCompleteness, CoverageObjectKey,
    CoverageStreamKey, EffectiveStateDimension, EffectiveStateEvidenceKind,
    EffectiveStateRevisionFact, EffectiveStateValueAuthority, FactProvenance, FactRevisionId,
    FactSemanticRevision, QualifiedUnknownReason, QualifiedValueQuality, SourceRecordId,
    UserInputOperation,
};
use crate::source::SourceRecordState;

pub(crate) const RUNTIME_REDUCED_STATE_DIGEST_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RevisionedEntityReduction {
    Unchanged,
    Upsert,
    Retract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RuntimeSemanticReductionError {
    #[error("invalid runtime semantic revision")]
    InvalidRevision,
    #[error("invalid runtime semantic source occurrence")]
    InvalidSource,
    #[error("duplicate runtime semantic fact identity")]
    DuplicateFact,
    #[error("runtime semantic reduction capacity exhausted")]
    CapacityExhausted,
}

fn validate_effective_state_entity(
    semantic: &FactSemanticRevision,
    revision: &EffectiveStateRevisionFact,
) -> Result<(), RuntimeSemanticReductionError> {
    revision
        .validate()
        .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    if semantic.semantic_revision_ref.fact_revision_id != semantic.fact_revision_id {
        return Err(RuntimeSemanticReductionError::InvalidRevision);
    }
    let revision_key = revision
        .semantic_revision_key()
        .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    let expected = FactRevisionId::derive(&semantic.fact_id, 1, &revision_key)
        .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    if expected != semantic.fact_revision_id {
        return Err(RuntimeSemanticReductionError::InvalidRevision);
    }
    Ok(())
}

/// Decide one effective-state revision using the RFC 012C
/// `RevisionedEntityCurrent` law.
///
/// Source/generation ordering is checked by the owning topology before this
/// function.  This function owns the topology-independent portion: normalized
/// semantic identity, exact-repeat suppression, complete-only retraction, and
/// current-value replacement.
pub(crate) fn reduce_effective_state_revision(
    current: Option<(&FactSemanticRevision, &EffectiveStateRevisionFact)>,
    incoming: (&FactSemanticRevision, &EffectiveStateRevisionFact),
) -> Result<RevisionedEntityReduction, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_effective_state_entity(incoming_semantic, incoming_revision)?;
    if let Some((current_semantic, current_revision)) = current {
        validate_effective_state_entity(current_semantic, current_revision)?;
        if current_semantic.fact_id != incoming_semantic.fact_id {
            return Err(RuntimeSemanticReductionError::InvalidRevision);
        }
        if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
            return Ok(RevisionedEntityReduction::Unchanged);
        }
    }
    match incoming_revision.operation {
        UserInputOperation::Retract
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            Ok(RevisionedEntityReduction::Unchanged)
        }
        UserInputOperation::Retract => Ok(RevisionedEntityReduction::Retract),
        UserInputOperation::Upsert => Ok(RevisionedEntityReduction::Upsert),
    }
}

/// Path-free canonical source occurrence used by both durable and scoped
/// reduced-state digests.  Host observation time, delivery phase, numeric
/// catalog IDs, and attachment-local tokens are intentionally absent.
#[derive(Clone, Copy)]
pub(crate) struct RuntimeSemanticSourceRef<'a> {
    pub adapter_id: &'a AdapterId,
    pub source_instance_key: &'a CanonicalSourceInstanceKey,
    pub stream_key: &'a CoverageStreamKey,
    pub object_key: &'a CoverageObjectKey,
    pub source_record_id: SourceRecordId,
    pub provenance: &'a FactProvenance,
    pub generation: u64,
    pub cursor_start: &'a [u8],
    pub cursor_end: &'a [u8],
    pub payload_hash: &'a [u8; 32],
    pub media_type: &'a str,
    pub state: SourceRecordState,
}

#[derive(Clone, Copy)]
pub(crate) struct EffectiveStateReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a EffectiveStateRevisionFact,
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn validate_and_hash_source(
    hasher: &mut blake3::Hasher,
    entity: &EffectiveStateReducedDigestEntity<'_>,
) -> Result<(), RuntimeSemanticReductionError> {
    validate_effective_state_entity(entity.semantic, entity.revision)?;
    let source = entity.source;
    if source.generation == 0
        || source.source_record_id != entity.semantic.source_record_id
        || source.provenance.generation != source.generation
        || source.provenance.cursor_start.as_slice() != source.cursor_start
        || source.provenance.cursor_end.as_slice() != source.cursor_end
        || &source.provenance.record_hash != source.payload_hash
    {
        return Err(RuntimeSemanticReductionError::InvalidSource);
    }
    hash_component(hasher, entity.semantic.fact_id.as_bytes());
    hasher.update(
        &entity
            .semantic
            .semantic_revision_ref
            .semantic_reference_contract_version
            .to_be_bytes(),
    );
    hash_component(hasher, entity.semantic.fact_revision_id.as_bytes());
    hash_component(hasher, entity.semantic.source_record_id.as_bytes());
    hash_component(hasher, source.adapter_id.as_str().as_bytes());
    hash_component(hasher, source.source_instance_key.as_bytes());
    hash_component(hasher, source.stream_key.as_bytes());
    hash_component(hasher, source.object_key.as_bytes());
    hasher.update(&source.generation.to_be_bytes());
    hash_component(hasher, source.cursor_start);
    hash_component(hasher, source.cursor_end);
    hasher.update(source.payload_hash);
    hash_component(hasher, source.media_type.as_bytes());
    hasher.update(&[match source.state {
        SourceRecordState::Present => 1,
        SourceRecordState::Absent => 2,
    }]);
    Ok(())
}

/// Compute the canonical current-state digest for `runtime.effective-state`.
/// Input order is irrelevant; duplicate fact identities fail closed.
pub(crate) fn effective_state_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = EffectiveStateReducedDigestEntity<'a>>,
) -> Result<[u8; 32], RuntimeSemanticReductionError> {
    let mut entities = entities.into_iter().collect::<Vec<_>>();
    entities.sort_unstable_by_key(|entity| entity.semantic.fact_id);
    if entities
        .windows(2)
        .any(|pair| pair[0].semantic.fact_id == pair[1].semantic.fact_id)
    {
        return Err(RuntimeSemanticReductionError::DuplicateFact);
    }
    let entity_count = u64::try_from(entities.len())
        .map_err(|_| RuntimeSemanticReductionError::CapacityExhausted)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/replacement-semantic-digest\0");
    hasher.update(&RUNTIME_REDUCED_STATE_DIGEST_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, b"runtime.effective-state");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_and_hash_source(&mut hasher, entity)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hasher.update(&[match entity.revision.dimension {
            EffectiveStateDimension::Model => 1,
            EffectiveStateDimension::Effort => 2,
            EffectiveStateDimension::SessionMode => 3,
            EffectiveStateDimension::PermissionMode => 4,
        }]);
        match entity.revision.value.value.as_ref() {
            Some(value) => {
                hasher.update(&[1]);
                hash_component(&mut hasher, value.as_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        }
        hasher.update(&[match entity.revision.value.quality {
            QualifiedValueQuality::Exact => 1,
            QualifiedValueQuality::NativeClaimed => 2,
            QualifiedValueQuality::Derived => 3,
            QualifiedValueQuality::Estimated => 4,
            QualifiedValueQuality::Unknown => 5,
        }]);
        hasher.update(&[match entity.revision.value.authority {
            EffectiveStateValueAuthority::NativeConfiguration => 1,
            EffectiveStateValueAuthority::NativeResponse => 2,
            EffectiveStateValueAuthority::NativeTransition => 3,
        }]);
        hasher.update(&[match entity.revision.value.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        }]);
        hasher.update(&[match entity.revision.value.unknown_reason {
            None => 0,
            Some(QualifiedUnknownReason::Missing) => 1,
            Some(QualifiedUnknownReason::Unsupported) => 2,
            Some(QualifiedUnknownReason::Withheld) => 3,
            Some(QualifiedUnknownReason::NotYetObserved) => 4,
            Some(QualifiedUnknownReason::Ambiguous) => 5,
            Some(QualifiedUnknownReason::Malformed) => 6,
        }]);
        match entity.revision.value.effective_at {
            Some(effective_at) => {
                hasher.update(&[1]);
                hasher.update(&effective_at.to_be_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        }
        hash_component(
            &mut hasher,
            entity.revision.value.provenance.native_field.as_bytes(),
        );
        hasher.update(
            &entity
                .revision
                .value
                .provenance
                .normalization_contract_version
                .to_be_bytes(),
        );
        hasher.update(&[match entity.revision.evidence_kind {
            EffectiveStateEvidenceKind::ConfiguredIntent => 1,
            EffectiveStateEvidenceKind::ResponseObserved => 2,
            EffectiveStateEvidenceKind::NativeTransition => 3,
        }]);
        hasher.update(&[match entity.revision.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        }]);
        hasher.update(&[match entity.revision.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        }]);
    }
    Ok(*hasher.finalize().as_bytes())
}
