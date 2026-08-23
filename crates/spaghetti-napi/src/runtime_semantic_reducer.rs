//! Topology-neutral RFC 012C revisioned-entity reduction primitives.
//!
//! Durable ingestion and scoped observation carry different delivery state,
//! but they must make the same semantic decision and publish the same reduced
//! state digest at an equal source/family coverage vector.  This module owns
//! that shared law; it deliberately contains no database or observer types.

use crate::adapter::{
    ActorAffiliationRevisionFact, ActorRunRevisionFact, ContentBlockRevisionFact,
    ContractCompleteness, EffectiveStateRevisionFact, Fact, FactRevisionId, FactSemanticRevision,
    MessageRevisionFact, NativeRuntimeMarkerRevisionFact, PlanRevisionFact, SemanticRevisionRef,
    TaskLifecycleState, TaskRevisionFact, ToolRevisionFact, UsageRevisionV2Fact,
    UserInputLifecycleState, UserInputOperation, UserInputQuestion, UserInputRequestRevisionFact,
};

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

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

/// Closed set of RFC 012C fact families accepted by the topology-neutral
/// reducer and downstream reconciliation boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuntimeSemanticFamily {
    ActorRun,
    ActorAffiliation,
    UsageV2,
    UserInputRequest,
    Message,
    ContentBlock,
    NativeMarker,
    Task,
    Plan,
    Tool,
    EffectiveState,
}

impl RuntimeSemanticFamily {
    pub(crate) const VERSION: u32 = 1;

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ActorRun => "runtime.actor-run",
            Self::ActorAffiliation => "runtime.actor-affiliation",
            Self::UsageV2 => "runtime.usage-v2",
            Self::UserInputRequest => "runtime.user-input-request",
            Self::Message => "runtime.message",
            Self::ContentBlock => "runtime.content-block",
            Self::NativeMarker => "runtime.native-marker",
            Self::Task => "runtime.task",
            Self::Plan => "runtime.plan",
            Self::Tool => "runtime.tool",
            Self::EffectiveState => "runtime.effective-state",
        }
    }
}

/// One already-typed current revision used to compute a topology-neutral
/// replacement-state digest. Native payloads and delivery/storage coordinates
/// are deliberately absent.
#[derive(Clone, Copy)]
pub(crate) struct RuntimeReplacementDigestEntity<'a> {
    pub semantic: &'a FactSemanticRevision,
    pub revision: &'a Fact,
}

fn runtime_fact_revision_key(
    semantic: &FactSemanticRevision,
    revision: &Fact,
) -> Result<(RuntimeSemanticFamily, [u8; 32]), RuntimeSemanticReductionError> {
    let (family, key) = match revision {
        Fact::ActorRunRevision(revision) => {
            validate_actor_run_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::ActorRun,
                revision.semantic_revision_key(),
            )
        }
        Fact::ActorAffiliationRevision(revision) => {
            validate_actor_affiliation_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::ActorAffiliation,
                revision.semantic_revision_key(),
            )
        }
        Fact::UsageRevisionV2(revision) => {
            validate_usage_v2_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::UsageV2,
                revision.semantic_revision_key(),
            )
        }
        Fact::UserInputRequestRevision(revision) => {
            validate_user_input_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::UserInputRequest,
                revision.semantic_revision_key(),
            )
        }
        Fact::MessageRevision(revision) => {
            validate_message_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::Message,
                revision.semantic_revision_key(),
            )
        }
        Fact::ContentBlockRevision(revision) => {
            validate_content_block_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::ContentBlock,
                revision.semantic_revision_key(),
            )
        }
        Fact::NativeRuntimeMarkerRevision(revision) => {
            validate_native_marker_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::NativeMarker,
                revision.semantic_revision_key(),
            )
        }
        Fact::TaskRevision(revision) => {
            validate_task_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::Task,
                revision.semantic_revision_key(),
            )
        }
        Fact::PlanRevision(revision) => {
            validate_plan_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::Plan,
                revision.semantic_revision_key(),
            )
        }
        Fact::ToolRevision(revision) => {
            validate_tool_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::Tool,
                revision.semantic_revision_key(),
            )
        }
        Fact::EffectiveStateRevision(revision) => {
            validate_effective_state_entity(semantic, revision)?;
            (
                RuntimeSemanticFamily::EffectiveState,
                revision.semantic_revision_key(),
            )
        }
        _ => return Err(RuntimeSemanticReductionError::InvalidRevision),
    };
    let key = key.map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    Ok((family, key))
}

pub(crate) fn validate_runtime_fact_revision(
    semantic: &FactSemanticRevision,
    revision: &Fact,
) -> Result<RuntimeSemanticFamily, RuntimeSemanticReductionError> {
    runtime_fact_revision_key(semantic, revision).map(|(family, _)| family)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RuntimeFactReduction {
    Unchanged,
    Upsert {
        semantic: FactSemanticRevision,
        revision: Box<Fact>,
    },
    Retract,
}

fn rebind_runtime_fact_revision(
    incoming_semantic: &FactSemanticRevision,
    revision: Fact,
) -> Result<(FactSemanticRevision, Fact), RuntimeSemanticReductionError> {
    let revision_key = match &revision {
        Fact::ActorRunRevision(revision) => revision.semantic_revision_key(),
        Fact::ActorAffiliationRevision(revision) => revision.semantic_revision_key(),
        Fact::UsageRevisionV2(revision) => revision.semantic_revision_key(),
        Fact::UserInputRequestRevision(revision) => revision.semantic_revision_key(),
        Fact::MessageRevision(revision) => revision.semantic_revision_key(),
        Fact::ContentBlockRevision(revision) => revision.semantic_revision_key(),
        Fact::NativeRuntimeMarkerRevision(revision) => revision.semantic_revision_key(),
        Fact::TaskRevision(revision) => revision.semantic_revision_key(),
        Fact::PlanRevision(revision) => revision.semantic_revision_key(),
        Fact::ToolRevision(revision) => revision.semantic_revision_key(),
        Fact::EffectiveStateRevision(revision) => revision.semantic_revision_key(),
        _ => return Err(RuntimeSemanticReductionError::InvalidRevision),
    }
    .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    let fact_revision_id = FactRevisionId::derive(
        &incoming_semantic.fact_id,
        RuntimeSemanticFamily::VERSION,
        &revision_key,
    )
    .map_err(|_| RuntimeSemanticReductionError::InvalidRevision)?;
    let semantic = FactSemanticRevision {
        source_record_id: incoming_semantic.source_record_id,
        fact_id: incoming_semantic.fact_id,
        fact_revision_id,
        semantic_revision_ref: SemanticRevisionRef::new(fact_revision_id),
    };
    validate_runtime_fact_revision(&semantic, &revision)?;
    Ok((semantic, revision))
}

fn simple_runtime_reduction(
    reduction: RevisionedEntityReduction,
    incoming_semantic: &FactSemanticRevision,
    incoming_revision: Fact,
) -> RuntimeFactReduction {
    match reduction {
        RevisionedEntityReduction::Unchanged => RuntimeFactReduction::Unchanged,
        RevisionedEntityReduction::Upsert => RuntimeFactReduction::Upsert {
            semantic: *incoming_semantic,
            revision: Box::new(incoming_revision),
        },
        RevisionedEntityReduction::Retract => RuntimeFactReduction::Retract,
    }
}

fn value_runtime_reduction<T>(
    reduction: RevisionedEntityValueReduction<T>,
    incoming_semantic: &FactSemanticRevision,
    wrap: impl FnOnce(T) -> Fact,
) -> Result<RuntimeFactReduction, RuntimeSemanticReductionError> {
    match reduction {
        RevisionedEntityValueReduction::Unchanged => Ok(RuntimeFactReduction::Unchanged),
        RevisionedEntityValueReduction::Retract => Ok(RuntimeFactReduction::Retract),
        RevisionedEntityValueReduction::Upsert(revision) => {
            let (semantic, revision) =
                rebind_runtime_fact_revision(incoming_semantic, wrap(revision))?;
            Ok(RuntimeFactReduction::Upsert {
                semantic,
                revision: Box::new(revision),
            })
        }
    }
}

/// Apply the same typed family law used by durable ingestion and scoped
/// projection to one downstream transition. A partial enrichment may produce
/// a newly bound merged revision; partial retractions remain unchanged.
pub(crate) fn reduce_runtime_fact_revision(
    current: Option<(&FactSemanticRevision, &Fact)>,
    incoming: (&FactSemanticRevision, &Fact),
) -> Result<RuntimeFactReduction, RuntimeSemanticReductionError> {
    let (incoming_semantic, incoming_revision) = incoming;
    match incoming_revision {
        Fact::ActorRunRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::ActorRunRevision(revision))) => Some((semantic, revision)),
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            Ok(simple_runtime_reduction(
                reduce_actor_run_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::ActorRunRevision(incoming_revision.clone()),
            ))
        }
        Fact::ActorAffiliationRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::ActorAffiliationRevision(revision))) => {
                    Some((semantic, revision))
                }
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            Ok(simple_runtime_reduction(
                reduce_actor_affiliation_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::ActorAffiliationRevision(incoming_revision.clone()),
            ))
        }
        Fact::UsageRevisionV2(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::UsageRevisionV2(revision))) => Some((semantic, revision)),
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            Ok(simple_runtime_reduction(
                reduce_usage_v2_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::UsageRevisionV2(incoming_revision.clone()),
            ))
        }
        Fact::UserInputRequestRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::UserInputRequestRevision(revision))) => {
                    Some((semantic, revision))
                }
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            value_runtime_reduction(
                reduce_user_input_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::UserInputRequestRevision,
            )
        }
        Fact::MessageRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::MessageRevision(revision))) => Some((semantic, revision)),
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            value_runtime_reduction(
                reduce_message_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::MessageRevision,
            )
        }
        Fact::ContentBlockRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::ContentBlockRevision(revision))) => {
                    Some((semantic, revision))
                }
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            Ok(simple_runtime_reduction(
                reduce_content_block_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::ContentBlockRevision(incoming_revision.clone()),
            ))
        }
        Fact::NativeRuntimeMarkerRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::NativeRuntimeMarkerRevision(revision))) => {
                    Some((semantic, revision))
                }
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            Ok(simple_runtime_reduction(
                reduce_native_marker_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::NativeRuntimeMarkerRevision(incoming_revision.clone()),
            ))
        }
        Fact::TaskRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::TaskRevision(revision))) => Some((semantic, revision)),
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            value_runtime_reduction(
                reduce_task_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::TaskRevision,
            )
        }
        Fact::PlanRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::PlanRevision(revision))) => Some((semantic, revision)),
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            value_runtime_reduction(
                reduce_plan_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::PlanRevision,
            )
        }
        Fact::ToolRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::ToolRevision(revision))) => Some((semantic, revision)),
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            value_runtime_reduction(
                reduce_tool_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::ToolRevision,
            )
        }
        Fact::EffectiveStateRevision(incoming_revision) => {
            let current = match current {
                Some((semantic, Fact::EffectiveStateRevision(revision))) => {
                    Some((semantic, revision))
                }
                Some(_) => return Err(RuntimeSemanticReductionError::InvalidRevision),
                None => None,
            };
            Ok(simple_runtime_reduction(
                reduce_effective_state_revision(current, (incoming_semantic, incoming_revision))?,
                incoming_semantic,
                Fact::EffectiveStateRevision(incoming_revision.clone()),
            ))
        }
        _ => Err(RuntimeSemanticReductionError::InvalidRevision),
    }
}

/// Digest the current replacement set for one runtime family. The revision
/// key already binds the complete typed value; this outer digest binds the
/// canonical fact/source/revision identities and rejects mixed families or
/// duplicate fact identities.
pub(crate) fn runtime_replacement_state_digest<'a>(
    family: RuntimeSemanticFamily,
    entities: impl IntoIterator<Item = RuntimeReplacementDigestEntity<'a>>,
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
    hasher.update(b"spaghetti/rfc012c/runtime-replacement-state-v1\0");
    hasher.update(&RUNTIME_REDUCED_STATE_DIGEST_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, family.as_str().as_bytes());
    hasher.update(&RuntimeSemanticFamily::VERSION.to_be_bytes());
    hasher.update(&entity_count.to_be_bytes());
    for entity in entities {
        let (entity_family, revision_key) =
            runtime_fact_revision_key(entity.semantic, entity.revision)?;
        if entity_family != family {
            return Err(RuntimeSemanticReductionError::InvalidRevision);
        }
        hash_component(&mut hasher, entity.semantic.fact_id.as_bytes());
        hasher.update(
            &entity
                .semantic
                .semantic_revision_ref
                .semantic_reference_contract_version
                .to_be_bytes(),
        );
        hash_component(&mut hasher, entity.semantic.fact_revision_id.as_bytes());
        hash_component(&mut hasher, entity.semantic.source_record_id.as_bytes());
        hash_component(&mut hasher, &revision_key);
    }
    Ok(*hasher.finalize().as_bytes())
}
