//! Crate-private RFC 012C durable/scoped usage merge.
//!
//! Reconciles a durable usage-v2 contribution list with ordered scoped observer
//! upsert/retract events. Overlay retirement is coverage-gated. Consumers never
//! parse native JSON payloads.

use std::collections::{BTreeMap, BTreeSet};

use crate::adapter::{
    compare_coverage, CanonicalEntityKey, CanonicalFactId, ContractCompleteness,
    CoverageComparison, CoverageSetCompleteness, SemanticContractError, SemanticRevisionRef,
    SourceCoverageSet, UsageRevisionV2Fact,
};
use crate::semantic_contract::{
    decode_rfc012c_interaction_v1, InteractionFixtureWire, InteractionLifecycleSlotWire,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SemanticMergeError {
    #[error("invalid durable/live usage merge: {0}")]
    Invalid(String),
}

impl SemanticMergeError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl From<SemanticContractError> for SemanticMergeError {
    fn from(error: SemanticContractError) -> Self {
        Self::Invalid(error.to_string())
    }
}

/// One durable usage-v2 contribution keyed by canonical fact identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableUsageContribution {
    pub fact_id: CanonicalFactId,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub revision: UsageRevisionV2Fact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedUsageOperation {
    Upsert,
    Retract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedUsageRetraction {
    Reset {
        old_generation: u64,
        new_generation: u64,
    },
    SourceDeleted {
        generation: u64,
    },
}

/// One occurrence-scoped observer event. `event_id` is the delivery
/// idempotency key; `semantic_revision_ref` is the RFC 012A join identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedUsageObserverEvent {
    pub event_id: String,
    pub fact_id: CanonicalFactId,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub operation: ScopedUsageOperation,
    pub retraction: Option<ScopedUsageRetraction>,
    pub revision: UsageRevisionV2Fact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayDisposition {
    Retired,
    Retained { stale: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergedContributionOrigin {
    Durable,
    Overlay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergedUsageContribution {
    pub fact_id: CanonicalFactId,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub revision: UsageRevisionV2Fact,
    pub origin: MergedContributionOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveredObserverOccurrence {
    pub event_id: String,
    pub fact_id: CanonicalFactId,
    pub semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableLiveUsageMerge {
    pub contributions: Vec<MergedUsageContribution>,
    pub overlay: OverlayDisposition,
    pub delivered_observer_occurrences: Vec<DeliveredObserverOccurrence>,
}

struct OverlayReduction {
    current: BTreeMap<CanonicalFactId, (SemanticRevisionRef, UsageRevisionV2Fact)>,
    retracted: BTreeSet<CanonicalFactId>,
    overlay_order: Vec<CanonicalFactId>,
    delivered: Vec<DeliveredObserverOccurrence>,
}

fn is_generation_retraction(event: &ScopedUsageObserverEvent) -> bool {
    matches!(
        event.operation,
        ScopedUsageOperation::Retract
            if matches!(
                event.retraction,
                Some(ScopedUsageRetraction::Reset { .. } | ScopedUsageRetraction::SourceDeleted { .. })
            )
    )
}

fn reduce_observer_events(
    events: &[ScopedUsageObserverEvent],
) -> Result<OverlayReduction, SemanticMergeError> {
    let mut seen_event_ids = BTreeSet::new();
    let mut unique = Vec::new();
    for event in events {
        if event.event_id.is_empty() {
            return Err(SemanticMergeError::invalid(
                "observer event_id must be a non-empty occurrence identity",
            ));
        }
        if seen_event_ids.insert(event.event_id.clone()) {
            unique.push(event);
        }
    }

    let mut resets = Vec::new();
    let mut replay = Vec::new();
    for event in unique {
        if is_generation_retraction(event) {
            resets.push(event);
        } else {
            replay.push(event);
        }
    }

    let mut current = BTreeMap::new();
    let mut retracted = BTreeSet::new();
    let mut overlay_order = Vec::new();
    let mut delivered = Vec::new();
    for event in resets.into_iter().chain(replay) {
        delivered.push(DeliveredObserverOccurrence {
            event_id: event.event_id.clone(),
            fact_id: event.fact_id,
            semantic_revision_ref: event.semantic_revision_ref,
        });
        match event.operation {
            ScopedUsageOperation::Retract => {
                current.remove(&event.fact_id);
                retracted.insert(event.fact_id);
            }
            ScopedUsageOperation::Upsert => {
                retracted.remove(&event.fact_id);
                if !current.contains_key(&event.fact_id) {
                    overlay_order.push(event.fact_id);
                }
                current.insert(
                    event.fact_id,
                    (event.semantic_revision_ref, event.revision.clone()),
                );
            }
        }
    }

    Ok(OverlayReduction {
        current,
        retracted,
        overlay_order,
        delivered,
    })
}

fn overlay_disposition(
    observer_coverage: &SourceCoverageSet,
    durable_coverage: &SourceCoverageSet,
) -> Result<OverlayDisposition, SemanticMergeError> {
    let comparison = compare_coverage(observer_coverage, durable_coverage)?;
    if observer_coverage.completeness == CoverageSetCompleteness::Complete
        && matches!(
            comparison,
            CoverageComparison::Equal | CoverageComparison::Dominates | CoverageComparison::Behind
        )
    {
        return Ok(OverlayDisposition::Retired);
    }
    Ok(OverlayDisposition::Retained { stale: true })
}

fn assemble_contributions(
    durable: &[DurableUsageContribution],
    overlay: &OverlayReduction,
    disposition: OverlayDisposition,
) -> Vec<MergedUsageContribution> {
    if matches!(disposition, OverlayDisposition::Retired) {
        return durable
            .iter()
            .map(|item| MergedUsageContribution {
                fact_id: item.fact_id,
                semantic_revision_ref: item.semantic_revision_ref,
                revision: item.revision.clone(),
                origin: MergedContributionOrigin::Durable,
            })
            .collect();
    }

    let mut seen = BTreeSet::new();
    let mut contributions = Vec::new();
    for item in durable {
        if overlay.retracted.contains(&item.fact_id) && !overlay.current.contains_key(&item.fact_id)
        {
            continue;
        }
        if let Some((semantic_revision_ref, revision)) = overlay.current.get(&item.fact_id) {
            contributions.push(MergedUsageContribution {
                fact_id: item.fact_id,
                semantic_revision_ref: *semantic_revision_ref,
                revision: revision.clone(),
                origin: MergedContributionOrigin::Overlay,
            });
        } else {
            contributions.push(MergedUsageContribution {
                fact_id: item.fact_id,
                semantic_revision_ref: item.semantic_revision_ref,
                revision: item.revision.clone(),
                origin: MergedContributionOrigin::Durable,
            });
        }
        seen.insert(item.fact_id);
    }
    for fact_id in &overlay.overlay_order {
        if seen.contains(fact_id) {
            continue;
        }
        let Some((semantic_revision_ref, revision)) = overlay.current.get(fact_id) else {
            continue;
        };
        contributions.push(MergedUsageContribution {
            fact_id: *fact_id,
            semantic_revision_ref: *semantic_revision_ref,
            revision: revision.clone(),
            origin: MergedContributionOrigin::Overlay,
        });
    }
    contributions
}

/// Merge durable usage-v2 contributions with scoped observer events.
///
/// Observer delivery is deduplicated by occurrence-scoped `event_id`. Generation
/// reset/deletion retractions apply before remaining replay. Complete comparable
/// scoped coverage retires the overlay; partial, unavailable, or incomparable
/// coverage retains it and marks it stale.
pub(crate) fn merge_durable_and_scoped_usage(
    durable: &[DurableUsageContribution],
    durable_coverage: &SourceCoverageSet,
    observer_events: &[ScopedUsageObserverEvent],
    observer_coverage: &SourceCoverageSet,
) -> Result<DurableLiveUsageMerge, SemanticMergeError> {
    durable_coverage.validate()?;
    observer_coverage.validate()?;
    let overlay = reduce_observer_events(observer_events)?;
    let disposition = overlay_disposition(observer_coverage, durable_coverage)?;
    Ok(DurableLiveUsageMerge {
        contributions: assemble_contributions(durable, &overlay, disposition),
        overlay: disposition,
        delivered_observer_occurrences: overlay.delivered,
    })
}

pub(crate) use crate::semantic_contract::{
    UserInputKind, UserInputLifecycleState, UserInputOperation, UserInputQuestion,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserInputLifecycleRevision {
    pub fact_id: CanonicalFactId,
    pub semantic_revision_ref: SemanticRevisionRef,
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub native_tool_use_id: String,
    pub kind: UserInputKind,
    pub questions: Vec<UserInputQuestion>,
    pub state: UserInputLifecycleState,
    pub operation: UserInputOperation,
    pub completeness: ContractCompleteness,
    pub result_reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InteractionContractFixture {
    pub family: String,
    pub family_version: u32,
    pub pending: UserInputLifecycleRevision,
    pub resolved: UserInputLifecycleRevision,
    pub failed: UserInputLifecycleRevision,
    pub cancelled: UserInputLifecycleRevision,
    pub retract: UserInputLifecycleRevision,
    pub partial: UserInputLifecycleRevision,
}

fn lifecycle_revision(
    wire: &InteractionFixtureWire,
    lifecycle: &InteractionLifecycleSlotWire,
) -> UserInputLifecycleRevision {
    UserInputLifecycleRevision {
        fact_id: wire.fact_id,
        semantic_revision_ref: lifecycle.semantic_revision_ref,
        session: wire.session,
        actor_run: wire.actor_run,
        native_tool_use_id: wire.native_tool_use_id.clone(),
        kind: wire.kind,
        questions: wire.questions.clone(),
        state: lifecycle.state,
        operation: lifecycle.operation,
        completeness: lifecycle.completeness,
        result_reference: lifecycle.result_reference.clone(),
    }
}

/// Parse the RFC 012C interaction lifecycle fixture for typed merge consumers.
pub(crate) fn parse_rfc012c_interaction_v1_json(
    json: &str,
) -> Result<InteractionContractFixture, SemanticMergeError> {
    let wire = decode_rfc012c_interaction_v1(json)
        .map_err(|error| SemanticMergeError::invalid(error.to_string()))?;
    Ok(InteractionContractFixture {
        family: wire.family.clone(),
        family_version: wire.family_version,
        pending: lifecycle_revision(&wire, &wire.pending),
        resolved: lifecycle_revision(&wire, &wire.resolved),
        failed: lifecycle_revision(&wire, &wire.failed),
        cancelled: lifecycle_revision(&wire, &wire.cancelled),
        retract: lifecycle_revision(&wire, &wire.retract),
        partial: lifecycle_revision(&wire, &wire.partial),
    })
}

#[cfg(test)]
mod tests;
