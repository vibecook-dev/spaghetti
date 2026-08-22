//! Topology-neutral RFC 012C revisioned-entity reduction primitives.
//!
//! Durable ingestion and scoped observation carry different delivery state,
//! but they must make the same semantic decision and publish the same reduced
//! state digest at an equal source/family coverage vector.  This module owns
//! that shared law; it deliberately contains no database or observer types.

use std::collections::{BTreeMap, BTreeSet};

use crate::adapter::{
    ActorAffiliationDimension, ActorAffiliationRevisionFact, ActorAffiliationState,
    ActorRunRevisionFact, ActorRunRole, AdapterId, CanonicalEntityKey, CanonicalSourceInstanceKey,
    ContentBlockRevisionFact, ContentBlockRevisionValue, ContractCompleteness, CoverageObjectKey,
    CoverageStreamKey, EffectiveStateDimension, EffectiveStateEvidenceKind,
    EffectiveStateRevisionFact, EffectiveStateValueAuthority, FactProvenance, FactRevisionId,
    FactSemanticRevision, MessageRevisionFact, MessageRevisionRole, NativeCompactionPhase,
    NativeProgressState, NativeQueueOperation, NativeRuntimeMarkerRevisionFact,
    NativeRuntimeMarkerValue, PlanRevisionFact, QualifiedUnknownReason, QualifiedValueQuality,
    SemanticRevisionRef, SourceRecordId, TaskLifecycleState, TaskRevisionFact, TimestampQuality,
    ToolRevisionFact, ToolRevisionKind, UsageRevisionV2Fact, UserInputKind,
    UserInputLifecycleState, UserInputOperation, UserInputQuestion, UserInputRequestRevisionFact,
};
use crate::source::SourceRecordState;

pub(crate) const RUNTIME_REDUCED_STATE_DIGEST_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RevisionedEntityReduction {
    Unchanged,
    Upsert,
    Retract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RevisionedEntityValueReduction<T> {
    Unchanged,
    Upsert(T),
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

fn validate_content_block_entity(
    semantic: &FactSemanticRevision,
    revision: &ContentBlockRevisionFact,
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

fn validate_native_marker_entity(
    semantic: &FactSemanticRevision,
    revision: &NativeRuntimeMarkerRevisionFact,
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

fn validate_user_input_entity(
    semantic: &FactSemanticRevision,
    revision: &UserInputRequestRevisionFact,
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

fn validate_message_entity(
    semantic: &FactSemanticRevision,
    revision: &MessageRevisionFact,
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

fn validate_plan_entity(
    semantic: &FactSemanticRevision,
    revision: &PlanRevisionFact,
) -> Result<(), RuntimeSemanticReductionError> {
    revision
        .validate()
        .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    if revision.completeness != ContractCompleteness::Complete && revision.owned_set.is_some() {
        return Err(RuntimeSemanticReductionError::InvalidRevision);
    }
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

fn validate_task_entity(
    semantic: &FactSemanticRevision,
    revision: &TaskRevisionFact,
) -> Result<(), RuntimeSemanticReductionError> {
    revision
        .validate()
        .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    if revision.completeness != ContractCompleteness::Complete && revision.owned_set.is_some() {
        return Err(RuntimeSemanticReductionError::InvalidRevision);
    }
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

fn validate_tool_entity(
    semantic: &FactSemanticRevision,
    revision: &ToolRevisionFact,
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

fn validate_actor_run_entity(
    semantic: &FactSemanticRevision,
    revision: &ActorRunRevisionFact,
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

fn validate_actor_affiliation_entity(
    semantic: &FactSemanticRevision,
    revision: &ActorAffiliationRevisionFact,
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

fn validate_usage_v2_entity(
    semantic: &FactSemanticRevision,
    revision: &UsageRevisionV2Fact,
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

/// Reduce one actor-run value while preserving the entity coordinates that
/// define its stable lineage. Parentage and native descriptive attributes may
/// be corrected, but an existing run cannot move to another session or change
/// root/child role under the same identity.
pub(crate) fn reduce_actor_run_revision(
    current: Option<(&FactSemanticRevision, &ActorRunRevisionFact)>,
    incoming: (&FactSemanticRevision, &ActorRunRevisionFact),
) -> Result<RevisionedEntityReduction, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_actor_run_entity(incoming_semantic, incoming_revision)?;
    if let Some((current_semantic, current_revision)) = current {
        validate_actor_run_entity(current_semantic, current_revision)?;
        if current_semantic.fact_id != incoming_semantic.fact_id
            || current_revision.actor_run != incoming_revision.actor_run
            || current_revision.session != incoming_revision.session
            || current_revision.role != incoming_revision.role
        {
            return Err(RuntimeSemanticReductionError::InvalidRevision);
        }
        if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
            return Ok(RevisionedEntityReduction::Unchanged);
        }
    }
    Ok(RevisionedEntityReduction::Upsert)
}

/// Reduce one affiliation value without allowing its stable relation identity
/// to be retargeted. Member/native attributes and explicit Present/Removed/
/// Unknown state remain revisioned values of the same actor/dimension/target
/// relation.
pub(crate) fn reduce_actor_affiliation_revision(
    current: Option<(&FactSemanticRevision, &ActorAffiliationRevisionFact)>,
    incoming: (&FactSemanticRevision, &ActorAffiliationRevisionFact),
) -> Result<RevisionedEntityReduction, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_actor_affiliation_entity(incoming_semantic, incoming_revision)?;
    if let Some((current_semantic, current_revision)) = current {
        validate_actor_affiliation_entity(current_semantic, current_revision)?;
        if current_semantic.fact_id != incoming_semantic.fact_id
            || current_revision.affiliation != incoming_revision.affiliation
            || current_revision.actor_run != incoming_revision.actor_run
            || current_revision.session != incoming_revision.session
            || current_revision.dimension != incoming_revision.dimension
            || current_revision.target != incoming_revision.target
        {
            return Err(RuntimeSemanticReductionError::InvalidRevision);
        }
        if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
            return Ok(RevisionedEntityReduction::Unchanged);
        }
    }
    Ok(RevisionedEntityReduction::Upsert)
}

/// Reduce one response-level usage contribution. Counters, qualifications,
/// attribution, correlation metadata, model/effort, and native time may be
/// corrected, but the contribution cannot be retargeted to another response
/// identity under the same canonical fact ID.
pub(crate) fn reduce_usage_v2_revision(
    current: Option<(&FactSemanticRevision, &UsageRevisionV2Fact)>,
    incoming: (&FactSemanticRevision, &UsageRevisionV2Fact),
) -> Result<RevisionedEntityReduction, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_usage_v2_entity(incoming_semantic, incoming_revision)?;
    if let Some((current_semantic, current_revision)) = current {
        validate_usage_v2_entity(current_semantic, current_revision)?;
        if current_semantic.fact_id != incoming_semantic.fact_id
            || current_revision.response_key != incoming_revision.response_key
            || current_revision.response_identity != incoming_revision.response_identity
            || current_revision.native_message_id != incoming_revision.native_message_id
        {
            return Err(RuntimeSemanticReductionError::InvalidRevision);
        }
        if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
            return Ok(RevisionedEntityReduction::Unchanged);
        }
    }
    Ok(RevisionedEntityReduction::Upsert)
}

fn merge_user_input_questions(
    current: &[UserInputQuestion],
    incoming: &[UserInputQuestion],
) -> Vec<UserInputQuestion> {
    let mut merged = current.to_vec();
    for question in incoming {
        if let Some(existing) = merged
            .iter_mut()
            .find(|known| known.prompt == question.prompt)
        {
            if existing.header.is_none() {
                existing.header = question.header.clone();
            }
            existing.multi_select |= question.multi_select;
            for option in &question.options {
                if let Some(existing_option) = existing
                    .options
                    .iter_mut()
                    .find(|known| known.label == option.label)
                {
                    if existing_option.description.is_none() {
                        existing_option.description = option.description.clone();
                    }
                    if existing_option.preview.is_none() {
                        existing_option.preview = option.preview.clone();
                    }
                } else {
                    existing.options.push(option.clone());
                }
            }
        } else {
            merged.push(question.clone());
        }
    }
    merged
}

/// Reduce one correlated user-input lifecycle revision without depending on
/// durable-store or scoped-observer coordinates.
///
/// Incomplete evidence may add typed question detail, but cannot change a known
/// lifecycle state, result, correlation identity, or interaction kind. A
/// incomplete terminal observation without a current entity is therefore not
/// sufficient to create terminal state.
pub(crate) fn reduce_user_input_revision(
    current: Option<(&FactSemanticRevision, &UserInputRequestRevisionFact)>,
    incoming: (&FactSemanticRevision, &UserInputRequestRevisionFact),
) -> Result<
    RevisionedEntityValueReduction<UserInputRequestRevisionFact>,
    RuntimeSemanticReductionError,
> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_user_input_entity(incoming_semantic, incoming_revision)?;
    let current = match current {
        Some((current_semantic, current_revision)) => {
            validate_user_input_entity(current_semantic, current_revision)?;
            if current_semantic.fact_id != incoming_semantic.fact_id
                || current_revision.session != incoming_revision.session
                || current_revision.actor_run != incoming_revision.actor_run
                || current_revision.native_tool_use_id != incoming_revision.native_tool_use_id
            {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
                return Ok(RevisionedEntityValueReduction::Unchanged);
            }
            Some((current_semantic, current_revision))
        }
        None => None,
    };

    match incoming_revision.operation {
        UserInputOperation::Retract
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            Ok(RevisionedEntityValueReduction::Unchanged)
        }
        UserInputOperation::Retract => Ok(RevisionedEntityValueReduction::Retract),
        UserInputOperation::Upsert
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            let Some((current_semantic, current_revision)) = current else {
                if incoming_revision.state != UserInputLifecycleState::Pending
                    || incoming_revision.result_reference.is_some()
                {
                    return Err(RuntimeSemanticReductionError::InvalidRevision);
                }
                return Ok(RevisionedEntityValueReduction::Upsert(
                    incoming_revision.clone(),
                ));
            };
            if current_revision.kind != incoming_revision.kind {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            let mut merged = incoming_revision.clone();
            merged.questions = merge_user_input_questions(
                &current_revision.questions,
                &incoming_revision.questions,
            );
            merged.state = current_revision.state;
            merged.result_reference = current_revision.result_reference.clone();
            merged
                .validate()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_key = merged
                .semantic_revision_key()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_revision_id =
                FactRevisionId::derive(&incoming_semantic.fact_id, 1, &merged_key)
                    .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            if merged_revision_id == current_semantic.fact_revision_id {
                Ok(RevisionedEntityValueReduction::Unchanged)
            } else {
                Ok(RevisionedEntityValueReduction::Upsert(merged))
            }
        }
        UserInputOperation::Upsert => Ok(RevisionedEntityValueReduction::Upsert(
            incoming_revision.clone(),
        )),
    }
}

fn merge_ordered_message_keys(current: &[String], incoming: &[String]) -> Vec<String> {
    let mut merged = current.to_vec();
    for key in incoming {
        if !merged.iter().any(|known| known == key) {
            merged.push(key.clone());
        }
    }
    merged
}

/// Reduce one current-generation message revision. An incomplete block list may
/// extend the current ordered set but cannot remove or reorder known blocks,
/// retarget the message, change its role, or retract it.
pub(crate) fn reduce_message_revision(
    current: Option<(&FactSemanticRevision, &MessageRevisionFact)>,
    incoming: (&FactSemanticRevision, &MessageRevisionFact),
) -> Result<RevisionedEntityValueReduction<MessageRevisionFact>, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_message_entity(incoming_semantic, incoming_revision)?;
    let current = match current {
        Some((current_semantic, current_revision)) => {
            validate_message_entity(current_semantic, current_revision)?;
            if current_semantic.fact_id != incoming_semantic.fact_id
                || current_revision.session != incoming_revision.session
                || current_revision.actor_run != incoming_revision.actor_run
                || current_revision.native_message_id != incoming_revision.native_message_id
            {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
                return Ok(RevisionedEntityValueReduction::Unchanged);
            }
            Some((current_semantic, current_revision))
        }
        None => None,
    };

    match incoming_revision.operation {
        UserInputOperation::Retract
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            Ok(RevisionedEntityValueReduction::Unchanged)
        }
        UserInputOperation::Retract => Ok(RevisionedEntityValueReduction::Retract),
        UserInputOperation::Upsert
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            let Some((current_semantic, current_revision)) = current else {
                return Ok(RevisionedEntityValueReduction::Upsert(
                    incoming_revision.clone(),
                ));
            };
            if current_revision.role != incoming_revision.role {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            let mut merged = incoming_revision.clone();
            merged.ordered_content_block_keys = merge_ordered_message_keys(
                &current_revision.ordered_content_block_keys,
                &incoming_revision.ordered_content_block_keys,
            );
            merged
                .validate()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_key = merged
                .semantic_revision_key()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_revision_id =
                FactRevisionId::derive(&incoming_semantic.fact_id, 1, &merged_key)
                    .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            if merged_revision_id == current_semantic.fact_revision_id {
                Ok(RevisionedEntityValueReduction::Unchanged)
            } else {
                Ok(RevisionedEntityValueReduction::Upsert(merged))
            }
        }
        UserInputOperation::Upsert => Ok(RevisionedEntityValueReduction::Upsert(
            incoming_revision.clone(),
        )),
    }
}

fn merge_ordered_plan_keys(current: &[String], incoming: &[String]) -> Vec<String> {
    let mut merged = current.to_vec();
    for key in incoming {
        if !merged.iter().any(|known| known == key) {
            merged.push(key.clone());
        }
    }
    merged
}

/// Reduce one plan revision independently of its durable or scoped delivery
/// topology. Incomplete evidence may extend a known ordered step set, but it
/// cannot remove or reorder steps, change the plan subject, retract the plan,
/// or carry complete-set absence authority.
pub(crate) fn reduce_plan_revision(
    current: Option<(&FactSemanticRevision, &PlanRevisionFact)>,
    incoming: (&FactSemanticRevision, &PlanRevisionFact),
) -> Result<RevisionedEntityValueReduction<PlanRevisionFact>, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_plan_entity(incoming_semantic, incoming_revision)?;
    let current = match current {
        Some((current_semantic, current_revision)) => {
            validate_plan_entity(current_semantic, current_revision)?;
            if current_semantic.fact_id != incoming_semantic.fact_id
                || current_revision.session != incoming_revision.session
                || current_revision.actor_run != incoming_revision.actor_run
                || current_revision.native_plan_id != incoming_revision.native_plan_id
            {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
                return Ok(RevisionedEntityValueReduction::Unchanged);
            }
            Some((current_semantic, current_revision))
        }
        None => None,
    };

    match incoming_revision.operation {
        UserInputOperation::Retract
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            Ok(RevisionedEntityValueReduction::Unchanged)
        }
        UserInputOperation::Retract => Ok(RevisionedEntityValueReduction::Retract),
        UserInputOperation::Upsert
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            let Some((current_semantic, current_revision)) = current else {
                return Ok(RevisionedEntityValueReduction::Upsert(
                    incoming_revision.clone(),
                ));
            };
            if current_revision.subject != incoming_revision.subject {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            let mut merged = incoming_revision.clone();
            merged.ordered_step_keys = merge_ordered_plan_keys(
                &current_revision.ordered_step_keys,
                &incoming_revision.ordered_step_keys,
            );
            merged.owned_set = None;
            merged
                .validate()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_key = merged
                .semantic_revision_key()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_revision_id =
                FactRevisionId::derive(&incoming_semantic.fact_id, 1, &merged_key)
                    .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            if merged_revision_id == current_semantic.fact_revision_id {
                Ok(RevisionedEntityValueReduction::Unchanged)
            } else {
                Ok(RevisionedEntityValueReduction::Upsert(merged))
            }
        }
        UserInputOperation::Upsert => Ok(RevisionedEntityValueReduction::Upsert(
            incoming_revision.clone(),
        )),
    }
}

/// Reduce one task lifecycle independently of storage/delivery topology.
/// Incomplete evidence cannot transition an existing lifecycle, replace its
/// subject, retract it, or claim complete-set absence.
pub(crate) fn reduce_task_revision(
    current: Option<(&FactSemanticRevision, &TaskRevisionFact)>,
    incoming: (&FactSemanticRevision, &TaskRevisionFact),
) -> Result<RevisionedEntityValueReduction<TaskRevisionFact>, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_task_entity(incoming_semantic, incoming_revision)?;
    let current = match current {
        Some((current_semantic, current_revision)) => {
            validate_task_entity(current_semantic, current_revision)?;
            if current_semantic.fact_id != incoming_semantic.fact_id
                || current_revision.session != incoming_revision.session
                || current_revision.actor_run != incoming_revision.actor_run
                || current_revision.native_task_id != incoming_revision.native_task_id
            {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
                return Ok(RevisionedEntityValueReduction::Unchanged);
            }
            Some((current_semantic, current_revision))
        }
        None => None,
    };

    match incoming_revision.operation {
        UserInputOperation::Retract
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            Ok(RevisionedEntityValueReduction::Unchanged)
        }
        UserInputOperation::Retract => Ok(RevisionedEntityValueReduction::Retract),
        UserInputOperation::Upsert
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            let Some((current_semantic, current_revision)) = current else {
                if matches!(
                    incoming_revision.state,
                    TaskLifecycleState::Completed
                        | TaskLifecycleState::Failed
                        | TaskLifecycleState::Cancelled
                        | TaskLifecycleState::Removed
                ) {
                    return Err(RuntimeSemanticReductionError::InvalidRevision);
                }
                return Ok(RevisionedEntityValueReduction::Upsert(
                    incoming_revision.clone(),
                ));
            };
            if current_revision.subject != incoming_revision.subject {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            let mut merged = incoming_revision.clone();
            merged.state = current_revision.state;
            merged.owned_set = None;
            merged
                .validate()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_key = merged
                .semantic_revision_key()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_revision_id =
                FactRevisionId::derive(&incoming_semantic.fact_id, 1, &merged_key)
                    .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            if merged_revision_id == current_semantic.fact_revision_id {
                Ok(RevisionedEntityValueReduction::Unchanged)
            } else {
                Ok(RevisionedEntityValueReduction::Upsert(merged))
            }
        }
        UserInputOperation::Upsert => Ok(RevisionedEntityValueReduction::Upsert(
            incoming_revision.clone(),
        )),
    }
}

/// Reduce one tool call/result entity with topology-independent correlation
/// semantics. Kind and tool name are stable for a native identity. Incomplete
/// evidence may add a previously unknown correlation, but cannot remove or
/// retarget a known one or retract the entity.
pub(crate) fn reduce_tool_revision(
    current: Option<(&FactSemanticRevision, &ToolRevisionFact)>,
    incoming: (&FactSemanticRevision, &ToolRevisionFact),
) -> Result<RevisionedEntityValueReduction<ToolRevisionFact>, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_tool_entity(incoming_semantic, incoming_revision)?;
    let current = match current {
        Some((current_semantic, current_revision)) => {
            validate_tool_entity(current_semantic, current_revision)?;
            if current_semantic.fact_id != incoming_semantic.fact_id
                || current_revision.session != incoming_revision.session
                || current_revision.actor_run != incoming_revision.actor_run
                || current_revision.native_tool_id != incoming_revision.native_tool_id
                || current_revision.kind != incoming_revision.kind
                || current_revision.tool_name != incoming_revision.tool_name
            {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            if current_semantic.fact_revision_id == incoming_semantic.fact_revision_id {
                return Ok(RevisionedEntityValueReduction::Unchanged);
            }
            Some((current_semantic, current_revision))
        }
        None => None,
    };

    match incoming_revision.operation {
        UserInputOperation::Retract
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            Ok(RevisionedEntityValueReduction::Unchanged)
        }
        UserInputOperation::Retract => Ok(RevisionedEntityValueReduction::Retract),
        UserInputOperation::Upsert
            if incoming_revision.completeness != ContractCompleteness::Complete =>
        {
            let Some((current_semantic, current_revision)) = current else {
                return Ok(RevisionedEntityValueReduction::Upsert(
                    incoming_revision.clone(),
                ));
            };
            if current_revision.correlated_native_id.is_some()
                && incoming_revision.correlated_native_id.is_some()
                && current_revision.correlated_native_id != incoming_revision.correlated_native_id
            {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
            let mut merged = incoming_revision.clone();
            if merged.correlated_native_id.is_none() {
                merged.correlated_native_id = current_revision.correlated_native_id.clone();
            }
            merged
                .validate()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_key = merged
                .semantic_revision_key()
                .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            let merged_revision_id =
                FactRevisionId::derive(&incoming_semantic.fact_id, 1, &merged_key)
                    .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
            if merged_revision_id == current_semantic.fact_revision_id {
                Ok(RevisionedEntityValueReduction::Unchanged)
            } else {
                Ok(RevisionedEntityValueReduction::Upsert(merged))
            }
        }
        UserInputOperation::Upsert => Ok(RevisionedEntityValueReduction::Upsert(
            incoming_revision.clone(),
        )),
    }
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

/// Decide one content-block revision using the RFC 012C
/// `CurrentGenerationLog` entity law.
///
/// The owning topology proves source generation and cursor progress. This
/// shared portion makes exact semantic replay occurrence-independent, treats
/// partial retractions as non-authoritative, and replaces one stable block
/// identity without appending duplicates.
pub(crate) fn reduce_content_block_revision(
    current: Option<(&FactSemanticRevision, &ContentBlockRevisionFact)>,
    incoming: (&FactSemanticRevision, &ContentBlockRevisionFact),
) -> Result<RevisionedEntityReduction, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_content_block_entity(incoming_semantic, incoming_revision)?;
    if let Some((current_semantic, current_revision)) = current {
        validate_content_block_entity(current_semantic, current_revision)?;
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

/// Decide one native marker revision using the RFC 012C
/// `CurrentGenerationLog` entity law.
///
/// Stable marker identity replaces its current value, exact replay is silent,
/// partial retraction is non-authoritative, and complete retraction removes the
/// entity. Source generation/cursor ordering remains topology-owned.
pub(crate) fn reduce_native_marker_revision(
    current: Option<(&FactSemanticRevision, &NativeRuntimeMarkerRevisionFact)>,
    incoming: (&FactSemanticRevision, &NativeRuntimeMarkerRevisionFact),
) -> Result<RevisionedEntityReduction, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    validate_native_marker_entity(incoming_semantic, incoming_revision)?;
    if let Some((current_semantic, current_revision)) = current {
        validate_native_marker_entity(current_semantic, current_revision)?;
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

#[derive(Clone, Copy)]
pub(crate) struct ContentBlockReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a ContentBlockRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct NativeMarkerReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a NativeRuntimeMarkerRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct UserInputReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a UserInputRequestRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct MessageReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a MessageRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct PlanReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a PlanRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct TaskReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a TaskRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct ToolReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a ToolRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct ActorRunReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a ActorRunRevisionFact,
}

#[derive(Clone, Copy)]
pub(crate) struct ActorAffiliationReducedDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub source: RuntimeSemanticSourceRef<'a>,
    pub revision: &'a ActorAffiliationRevisionFact,
}

struct ActorAffiliationDigestContext<'a> {
    session: CanonicalEntityKey,
    team: Option<&'a ActorAffiliationRevisionFact>,
    workflow: Option<&'a ActorAffiliationRevisionFact>,
    team_ambiguous: bool,
    workflow_ambiguous: bool,
    revision_refs: Vec<SemanticRevisionRef>,
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_optional_component(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_component(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_u64(hasher: &mut blake3::Hasher, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn validate_and_hash_semantic_source(
    hasher: &mut blake3::Hasher,
    semantic: &FactSemanticRevision,
    source: RuntimeSemanticSourceRef<'_>,
) -> Result<(), RuntimeSemanticReductionError> {
    if source.generation == 0
        || source.source_record_id != semantic.source_record_id
        || source.provenance.generation != source.generation
        || source.provenance.cursor_start.as_slice() != source.cursor_start
        || source.provenance.cursor_end.as_slice() != source.cursor_end
        || &source.provenance.record_hash != source.payload_hash
    {
        return Err(RuntimeSemanticReductionError::InvalidSource);
    }
    hash_component(hasher, semantic.fact_id.as_bytes());
    hasher.update(
        &semantic
            .semantic_revision_ref
            .semantic_reference_contract_version
            .to_be_bytes(),
    );
    hash_component(hasher, semantic.fact_revision_id.as_bytes());
    hash_component(hasher, semantic.source_record_id.as_bytes());
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

/// Compute the canonical current-state digest for `runtime.actor-run`.
/// Actor-run order, rather than delivery order or fact-hash order, preserves
/// the frozen replacement-family byte contract. Duplicate run or fact
/// identities fail closed.
pub(crate) fn actor_run_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = ActorRunReducedDigestEntity<'a>>,
) -> Result<[u8; 32], RuntimeSemanticReductionError> {
    let mut entities = entities.into_iter().collect::<Vec<_>>();
    entities.sort_unstable_by_key(|entity| entity.revision.actor_run);
    if entities
        .windows(2)
        .any(|pair| pair[0].revision.actor_run == pair[1].revision.actor_run)
    {
        return Err(RuntimeSemanticReductionError::DuplicateFact);
    }
    let mut fact_ids = BTreeSet::new();
    if entities
        .iter()
        .any(|entity| !fact_ids.insert(entity.semantic.fact_id))
    {
        return Err(RuntimeSemanticReductionError::DuplicateFact);
    }
    let entity_count = u64::try_from(entities.len())
        .map_err(|_| RuntimeSemanticReductionError::CapacityExhausted)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/replacement-semantic-digest\0");
    hasher.update(&RUNTIME_REDUCED_STATE_DIGEST_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, b"runtime.actor-run");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_actor_run_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hasher.update(&[match entity.revision.role {
            ActorRunRole::Root => 1,
            ActorRunRole::Child => 2,
        }]);
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .parent_actor_run
                .as_ref()
                .map(|key| key.as_bytes().as_slice()),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_session_id
                .as_deref()
                .map(str::as_bytes),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_actor_id
                .as_deref()
                .map(str::as_bytes),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_actor_type
                .as_deref()
                .map(str::as_bytes),
        );

        // The frozen v1 digest also binds the derived ActorRunRef projection.
        // Re-encode that projection from the validated common revision so a
        // topology-local wrapper cannot supply a divergent duplicate value.
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hasher.update(&[match entity.revision.role {
            ActorRunRole::Root => 1,
            ActorRunRole::Child => 2,
        }]);
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .parent_actor_run
                .as_ref()
                .map(|key| key.as_bytes().as_slice()),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_session_id
                .as_deref()
                .map(str::as_bytes),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_actor_id
                .as_deref()
                .map(str::as_bytes),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_actor_type
                .as_deref()
                .map(str::as_bytes),
        );
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Compute the canonical current-state digest for
/// `runtime.actor-affiliation`. The actor-level delivery context is derived
/// from the accepted common revisions, so neither durable nor scoped storage
/// can supply a divergent union overlay.
pub(crate) fn actor_affiliation_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = ActorAffiliationReducedDigestEntity<'a>>,
) -> Result<[u8; 32], RuntimeSemanticReductionError> {
    let mut entities = entities.into_iter().collect::<Vec<_>>();
    entities.sort_unstable_by_key(|entity| entity.revision.affiliation);
    if entities
        .windows(2)
        .any(|pair| pair[0].revision.affiliation == pair[1].revision.affiliation)
    {
        return Err(RuntimeSemanticReductionError::DuplicateFact);
    }
    let mut fact_ids = BTreeSet::new();
    if entities
        .iter()
        .any(|entity| !fact_ids.insert(entity.semantic.fact_id))
    {
        return Err(RuntimeSemanticReductionError::DuplicateFact);
    }

    let mut contexts = BTreeMap::<CanonicalEntityKey, ActorAffiliationDigestContext<'_>>::new();
    for entity in &entities {
        validate_actor_affiliation_entity(entity.semantic, entity.revision)?;
        let context = contexts
            .entry(entity.revision.actor_run)
            .or_insert_with(|| ActorAffiliationDigestContext {
                session: entity.revision.session,
                team: None,
                workflow: None,
                team_ambiguous: false,
                workflow_ambiguous: false,
                revision_refs: Vec::new(),
            });
        if context.session != entity.revision.session {
            return Err(RuntimeSemanticReductionError::InvalidRevision);
        }
        context
            .revision_refs
            .push(entity.semantic.semantic_revision_ref);
        match (entity.revision.dimension, entity.revision.state) {
            (ActorAffiliationDimension::Team, ActorAffiliationState::Present) => {
                if context.team.replace(entity.revision).is_some() {
                    context.team_ambiguous = true;
                }
            }
            (ActorAffiliationDimension::Workflow, ActorAffiliationState::Present) => {
                if context.workflow.replace(entity.revision).is_some() {
                    context.workflow_ambiguous = true;
                }
            }
            (ActorAffiliationDimension::Team, ActorAffiliationState::Unknown) => {
                context.team_ambiguous = true;
            }
            (ActorAffiliationDimension::Workflow, ActorAffiliationState::Unknown) => {
                context.workflow_ambiguous = true;
            }
            (_, ActorAffiliationState::Removed) => {}
        }
    }
    for context in contexts.values_mut() {
        context
            .revision_refs
            .sort_unstable_by_key(|reference| reference.fact_revision_id);
        if context
            .revision_refs
            .windows(2)
            .any(|pair| pair[0].fact_revision_id == pair[1].fact_revision_id)
        {
            return Err(RuntimeSemanticReductionError::InvalidRevision);
        }
        if context.team_ambiguous {
            context.team = None;
        }
        if context.workflow_ambiguous {
            context.workflow = None;
        }
    }

    let entity_count = u64::try_from(entities.len())
        .map_err(|_| RuntimeSemanticReductionError::CapacityExhausted)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/replacement-semantic-digest\0");
    hasher.update(&RUNTIME_REDUCED_STATE_DIGEST_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, b"runtime.actor-affiliation");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.affiliation.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hasher.update(&[match entity.revision.dimension {
            ActorAffiliationDimension::Team => 1,
            ActorAffiliationDimension::Workflow => 2,
        }]);
        hash_component(&mut hasher, entity.revision.target.as_bytes());
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .member
                .as_ref()
                .map(|key| key.as_bytes().as_slice()),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_target_id
                .as_deref()
                .map(str::as_bytes),
        );
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_member_id
                .as_deref()
                .map(str::as_bytes),
        );
        hasher.update(&[match entity.revision.state {
            ActorAffiliationState::Present => 1,
            ActorAffiliationState::Removed => 2,
            ActorAffiliationState::Unknown => 3,
        }]);
        match &entity.revision.effective_at {
            Some(timestamp) => {
                hasher.update(&[1]);
                hash_component(&mut hasher, timestamp.value.as_bytes());
                hasher.update(&[match timestamp.quality {
                    TimestampQuality::NativeExact => 1,
                    TimestampQuality::NativeApproximate => 2,
                    TimestampQuality::FileMetadataFallback => 3,
                    TimestampQuality::Derived => 4,
                }]);
            }
            None => {
                hasher.update(&[0]);
            }
        }

        let context = contexts
            .get(&entity.revision.actor_run)
            .ok_or(RuntimeSemanticReductionError::InvalidRevision)?;
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        let team = context.team;
        hash_optional_component(
            &mut hasher,
            team.map(|revision| revision.target.as_bytes().as_slice()),
        );
        hash_optional_component(
            &mut hasher,
            team.and_then(|revision| revision.native_target_id.as_deref())
                .map(str::as_bytes),
        );
        // team_name is intentionally absent in the v1 derived context.
        hash_optional_component(&mut hasher, None);
        hash_optional_component(
            &mut hasher,
            team.and_then(|revision| revision.member.as_ref())
                .map(|key| key.as_bytes().as_slice()),
        );
        let workflow = context.workflow;
        hash_optional_component(
            &mut hasher,
            workflow.map(|revision| revision.target.as_bytes().as_slice()),
        );
        hash_optional_component(
            &mut hasher,
            workflow
                .and_then(|revision| revision.native_target_id.as_deref())
                .map(str::as_bytes),
        );
        hasher.update(&[if context.team_ambiguous || context.workflow_ambiguous {
            3
        } else {
            2
        }]);
        hasher.update(&(context.revision_refs.len() as u64).to_be_bytes());
        for reference in &context.revision_refs {
            hasher.update(&reference.semantic_reference_contract_version.to_be_bytes());
            hash_component(&mut hasher, reference.fact_revision_id.as_bytes());
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Compute the canonical current-state digest for
/// `runtime.user-input-request`. Input order is irrelevant; duplicate fact
/// identities fail closed. The digest binds the fully reduced lifecycle value
/// and its path-free source occurrence.
pub(crate) fn user_input_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = UserInputReducedDigestEntity<'a>>,
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
    hash_component(&mut hasher, b"runtime.user-input-request");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_user_input_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.native_tool_use_id.as_bytes());
        hasher.update(&[match entity.revision.kind {
            UserInputKind::Choice => 1,
            UserInputKind::MultiChoice => 2,
            UserInputKind::FreeText => 3,
            UserInputKind::Mixed => 4,
        }]);
        hasher.update(&(entity.revision.questions.len() as u64).to_be_bytes());
        for question in &entity.revision.questions {
            hash_optional_component(&mut hasher, question.header.as_deref().map(str::as_bytes));
            hash_component(&mut hasher, question.prompt.as_bytes());
            hasher.update(&[u8::from(question.multi_select)]);
            hasher.update(&(question.options.len() as u64).to_be_bytes());
            for option in &question.options {
                hash_component(&mut hasher, option.label.as_bytes());
                hash_optional_component(
                    &mut hasher,
                    option.description.as_deref().map(str::as_bytes),
                );
                hash_optional_component(&mut hasher, option.preview.as_deref().map(str::as_bytes));
            }
        }
        hasher.update(&[match entity.revision.state {
            UserInputLifecycleState::Pending => 1,
            UserInputLifecycleState::Resolved => 2,
            UserInputLifecycleState::Failed => 3,
            UserInputLifecycleState::Cancelled => 4,
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
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .result_reference
                .as_deref()
                .map(str::as_bytes),
        );
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Compute the canonical current-state digest for `runtime.message`.
pub(crate) fn message_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = MessageReducedDigestEntity<'a>>,
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
    hash_component(&mut hasher, b"runtime.message");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_message_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.native_message_id.as_bytes());
        hasher.update(&[match entity.revision.role {
            MessageRevisionRole::User => 1,
            MessageRevisionRole::Assistant => 2,
            MessageRevisionRole::System => 3,
        }]);
        hasher.update(&(entity.revision.ordered_content_block_keys.len() as u64).to_be_bytes());
        for key in &entity.revision.ordered_content_block_keys {
            hash_component(&mut hasher, key.as_bytes());
        }
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

/// Compute the canonical current-state digest for `runtime.plan`.
pub(crate) fn plan_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = PlanReducedDigestEntity<'a>>,
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
    hash_component(&mut hasher, b"runtime.plan");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_plan_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.native_plan_id.as_bytes());
        hash_component(&mut hasher, entity.revision.subject.as_bytes());
        hasher.update(&(entity.revision.ordered_step_keys.len() as u64).to_be_bytes());
        for key in &entity.revision.ordered_step_keys {
            hash_component(&mut hasher, key.as_bytes());
        }
        hasher.update(&[match entity.revision.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        }]);
        hasher.update(&[match entity.revision.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        }]);
        match &entity.revision.owned_set {
            Some(owned_set) => {
                hasher.update(&[1]);
                hasher.update(&(owned_set.len() as u64).to_be_bytes());
                for member in owned_set {
                    hash_component(&mut hasher, member.as_bytes());
                }
            }
            None => {
                hasher.update(&[0]);
            }
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Compute the canonical current-state digest for `runtime.task`.
pub(crate) fn task_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = TaskReducedDigestEntity<'a>>,
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
    hash_component(&mut hasher, b"runtime.task");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_task_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.native_task_id.as_bytes());
        hash_component(&mut hasher, entity.revision.subject.as_bytes());
        hasher.update(&[match entity.revision.state {
            TaskLifecycleState::Created => 1,
            TaskLifecycleState::Updated => 2,
            TaskLifecycleState::Completed => 3,
            TaskLifecycleState::Failed => 4,
            TaskLifecycleState::Cancelled => 5,
            TaskLifecycleState::Removed => 6,
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
        match &entity.revision.owned_set {
            Some(owned_set) => {
                hasher.update(&[1]);
                hasher.update(&(owned_set.len() as u64).to_be_bytes());
                for member in owned_set {
                    hash_component(&mut hasher, member.as_bytes());
                }
            }
            None => {
                hasher.update(&[0]);
            }
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Compute the canonical current-state digest for `runtime.tool`.
pub(crate) fn tool_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = ToolReducedDigestEntity<'a>>,
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
    hash_component(&mut hasher, b"runtime.tool");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_tool_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.native_tool_id.as_bytes());
        hasher.update(&[match entity.revision.kind {
            ToolRevisionKind::Call => 1,
            ToolRevisionKind::Result => 2,
        }]);
        hash_component(&mut hasher, entity.revision.tool_name.as_bytes());
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .correlated_native_id
                .as_deref()
                .map(str::as_bytes),
        );
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
        validate_effective_state_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
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

/// Compute the canonical current-state digest for `runtime.content-block`.
/// Input order is irrelevant; duplicate fact identities fail closed. The
/// digest binds the full typed winner and its path-free source occurrence, so
/// a durable query and a scoped replacement can compare without sharing
/// storage or delivery coordinates.
pub(crate) fn content_block_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = ContentBlockReducedDigestEntity<'a>>,
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
    hash_component(&mut hasher, b"runtime.content-block");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_content_block_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.message.as_bytes());
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_content_block_id
                .as_deref()
                .map(str::as_bytes),
        );
        hasher.update(&entity.revision.ordinal.to_be_bytes());
        match &entity.revision.content {
            ContentBlockRevisionValue::Text { text } => {
                hasher.update(&[1]);
                hash_component(&mut hasher, text.as_bytes());
            }
            ContentBlockRevisionValue::Thinking { text, redacted } => {
                hasher.update(&[2]);
                hash_component(&mut hasher, text.as_bytes());
                hasher.update(&[u8::from(*redacted)]);
            }
            ContentBlockRevisionValue::ToolCall {
                tool_name,
                input_digest,
            } => {
                hasher.update(&[3]);
                hash_component(&mut hasher, tool_name.as_bytes());
                hasher.update(input_digest);
            }
            ContentBlockRevisionValue::ToolResult {
                content_digest,
                is_error,
            } => {
                hasher.update(&[4]);
                hasher.update(content_digest);
                hasher.update(&[u8::from(*is_error)]);
            }
            ContentBlockRevisionValue::Image {
                media_type,
                data_hash,
            } => {
                hasher.update(&[5]);
                hash_component(&mut hasher, media_type.as_bytes());
                hasher.update(data_hash);
            }
            ContentBlockRevisionValue::Document {
                media_type,
                data_hash,
            } => {
                hasher.update(&[6]);
                hash_component(&mut hasher, media_type.as_bytes());
                hasher.update(data_hash);
            }
            ContentBlockRevisionValue::NativeExtension {
                native_kind,
                value_digest,
            } => {
                hasher.update(&[7]);
                hash_component(&mut hasher, native_kind.as_bytes());
                hasher.update(value_digest);
            }
        }
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .native_tool_call_or_result_id
                .as_deref()
                .map(str::as_bytes),
        );
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

/// Compute the canonical current-state digest for `runtime.native-marker`.
/// Input order is irrelevant; duplicate fact identities fail closed. The
/// digest contains only typed marker values and path-free source occurrence.
pub(crate) fn native_marker_reduced_state_digest<'a>(
    entities: impl IntoIterator<Item = NativeMarkerReducedDigestEntity<'a>>,
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
    hash_component(&mut hasher, b"runtime.native-marker");
    hasher.update(&1_u32.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in &entities {
        validate_native_marker_entity(entity.semantic, entity.revision)?;
        validate_and_hash_semantic_source(&mut hasher, entity.semantic, entity.source)?;
        hash_component(&mut hasher, entity.revision.session.as_bytes());
        hash_component(&mut hasher, entity.revision.actor_run.as_bytes());
        hash_component(&mut hasher, entity.revision.native_marker_id.as_bytes());
        hash_optional_component(
            &mut hasher,
            entity
                .revision
                .correlated_native_id
                .as_deref()
                .map(str::as_bytes),
        );
        match &entity.revision.value {
            NativeRuntimeMarkerValue::Compaction {
                phase,
                trigger,
                pre_tokens,
            } => {
                hasher.update(&[1]);
                hasher.update(&[match phase {
                    NativeCompactionPhase::Started => 1,
                    NativeCompactionPhase::Boundary => 2,
                    NativeCompactionPhase::Completed => 3,
                    NativeCompactionPhase::Failed => 4,
                }]);
                hash_optional_component(&mut hasher, trigger.as_deref().map(str::as_bytes));
                hash_optional_u64(&mut hasher, *pre_tokens);
            }
            NativeRuntimeMarkerValue::Progress {
                state,
                completed,
                total,
                detail_digest,
            } => {
                hasher.update(&[2]);
                hasher.update(&[match state {
                    NativeProgressState::Pending => 1,
                    NativeProgressState::Active => 2,
                    NativeProgressState::Waiting => 3,
                    NativeProgressState::Completed => 4,
                    NativeProgressState::Failed => 5,
                    NativeProgressState::Cancelled => 6,
                }]);
                hash_optional_u64(&mut hasher, *completed);
                hash_optional_u64(&mut hasher, *total);
                hash_optional_component(
                    &mut hasher,
                    detail_digest.as_ref().map(|digest| digest.as_slice()),
                );
            }
            NativeRuntimeMarkerValue::Queue {
                operation,
                depth,
                item_digest,
            } => {
                hasher.update(&[3]);
                hasher.update(&[match operation {
                    NativeQueueOperation::Enqueue => 1,
                    NativeQueueOperation::Dequeue => 2,
                    NativeQueueOperation::Drain => 3,
                    NativeQueueOperation::Remove => 4,
                }]);
                hash_optional_u64(&mut hasher, *depth);
                hash_optional_component(
                    &mut hasher,
                    item_digest.as_ref().map(|digest| digest.as_slice()),
                );
            }
        }
        hasher.update(&[match entity.revision.quality {
            QualifiedValueQuality::Exact => 1,
            QualifiedValueQuality::NativeClaimed => 2,
            QualifiedValueQuality::Derived
            | QualifiedValueQuality::Estimated
            | QualifiedValueQuality::Unknown => {
                return Err(RuntimeSemanticReductionError::InvalidRevision);
            }
        }]);
        match entity.revision.effective_at {
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
            entity.revision.provenance.native_field.as_bytes(),
        );
        hasher.update(
            &entity
                .revision
                .provenance
                .normalization_contract_version
                .to_be_bytes(),
        );
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
