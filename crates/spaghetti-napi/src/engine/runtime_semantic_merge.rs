//! Crate-private RFC 012C durable/scoped semantic merge.
//!
//! Reconciles durable current state with ordered scoped observer upsert/retract
//! events. Overlay replacement is grouped by canonical fact identity;
//! `SemanticRevisionRef` is the revision join; `event_id` deduplicates delivery.
//! Overlay retirement is coverage-gated. The original usage-v2 adapter remains
//! while callers migrate to the closed all-family boundary. Consumers never
//! parse native JSON payloads.

use std::collections::{BTreeMap, BTreeSet};

use crate::adapter::{
    compare_coverage, CanonicalEntityKey, CanonicalFactId, ContractCompleteness,
    CoverageComparison, CoverageDomain, CoverageSetCompleteness, Fact, FactRevisionId,
    FactSemanticRevision, SemanticContractError, SemanticRevisionRef, SourceCoverageSet,
    UsageRevisionV2Fact,
};
use crate::runtime_semantic_reducer::{
    reduce_runtime_fact_revision, runtime_fact_declares_retraction,
    runtime_replacement_state_digest, validate_runtime_fact_revision, RuntimeFactReduction,
    RuntimeReplacementDigestEntity, RuntimeSemanticFamily,
};
use crate::semantic_contract::{
    decode_rfc012c_interaction_v1, InteractionFixtureWire, InteractionLifecycleSlotWire,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SemanticMergeError {
    #[error("invalid durable/live runtime semantic merge: {0}")]
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

const USAGE_V2_FAMILY: &str = "runtime.usage-v2";
const USAGE_V2_FAMILY_VERSION: u32 = 1;

fn validate_usage_revision_identity(
    fact_id: CanonicalFactId,
    semantic_revision_ref: SemanticRevisionRef,
    revision: &UsageRevisionV2Fact,
) -> Result<(), SemanticMergeError> {
    let revision_key = revision
        .semantic_revision_key()
        .map_err(|_| SemanticMergeError::invalid("usage revision is invalid"))?;
    let expected = FactRevisionId::derive(&fact_id, USAGE_V2_FAMILY_VERSION, &revision_key)
        .map(SemanticRevisionRef::new)
        .map_err(|_| SemanticMergeError::invalid("usage revision identity is invalid"))?;
    if semantic_revision_ref != expected {
        return Err(SemanticMergeError::invalid(
            "usage semantic revision reference does not bind its typed value",
        ));
    }
    Ok(())
}

fn validate_usage_coverage(coverage: &SourceCoverageSet) -> Result<(), SemanticMergeError> {
    coverage.validate()?;
    if coverage.coverage_domain
        != (CoverageDomain::FactFamily {
            family: USAGE_V2_FAMILY.to_string(),
            version: USAGE_V2_FAMILY_VERSION,
        })
    {
        return Err(SemanticMergeError::invalid(
            "usage merge requires runtime.usage-v2@1 fact-family coverage",
        ));
    }
    Ok(())
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
    let mut seen_event_ids = BTreeMap::new();
    let mut unique = Vec::new();
    for event in events {
        if event.event_id.is_empty() {
            return Err(SemanticMergeError::invalid(
                "observer event_id must be a non-empty occurrence identity",
            ));
        }
        validate_usage_revision_identity(
            event.fact_id,
            event.semantic_revision_ref,
            &event.revision,
        )?;
        match seen_event_ids.get(event.event_id.as_str()) {
            Some(previous) if *previous != event => {
                return Err(SemanticMergeError::invalid(
                    "one observer event_id cannot identify different event content",
                ));
            }
            Some(_) => {}
            None => {
                seen_event_ids.insert(event.event_id.as_str(), event);
                unique.push(event);
            }
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
        && durable_coverage.completeness == CoverageSetCompleteness::Complete
        && matches!(
            comparison,
            CoverageComparison::Equal | CoverageComparison::Behind
        )
    {
        return Ok(OverlayDisposition::Retired);
    }
    Ok(OverlayDisposition::Retained {
        stale: observer_coverage.completeness != CoverageSetCompleteness::Complete
            || durable_coverage.completeness == CoverageSetCompleteness::Complete
                && comparison != CoverageComparison::Dominates,
    })
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
/// Observer delivery is deduplicated by occurrence-scoped `event_id`. Overlay
/// replacement is grouped by canonical fact identity so A→B→A revises one
/// response; each contribution carries `SemanticRevisionRef` as the revision
/// join identity. Generation reset/deletion retractions apply before remaining
/// replay. Complete comparable scoped coverage retires the overlay; partial,
/// unavailable, or incomparable coverage retains it and marks it stale.
pub(crate) fn merge_durable_and_scoped_usage(
    durable: &[DurableUsageContribution],
    durable_coverage: &SourceCoverageSet,
    observer_events: &[ScopedUsageObserverEvent],
    observer_coverage: &SourceCoverageSet,
) -> Result<DurableLiveUsageMerge, SemanticMergeError> {
    validate_usage_coverage(durable_coverage)?;
    validate_usage_coverage(observer_coverage)?;
    let mut durable_fact_ids = BTreeSet::new();
    for contribution in durable {
        if !durable_fact_ids.insert(contribution.fact_id) {
            return Err(SemanticMergeError::invalid(
                "durable usage contains a duplicate canonical fact identity",
            ));
        }
        validate_usage_revision_identity(
            contribution.fact_id,
            contribution.semantic_revision_ref,
            &contribution.revision,
        )?;
    }
    let overlay = reduce_observer_events(observer_events)?;
    let disposition = overlay_disposition(observer_coverage, durable_coverage)?;
    Ok(DurableLiveUsageMerge {
        contributions: assemble_contributions(durable, &overlay, disposition),
        overlay: disposition,
        delivered_observer_occurrences: overlay.delivered,
    })
}

/// One current durable revision on the closed RFC 012C semantic boundary.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DurableRuntimeContribution {
    pub semantic: FactSemanticRevision,
    pub revision: Fact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedRuntimeOperation {
    Upsert,
    Retract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedRuntimeRetraction {
    Reset {
        old_generation: u64,
        new_generation: u64,
    },
    SourceDeleted {
        generation: u64,
    },
}

/// One occurrence-scoped, already-typed observer event. The common fact value
/// is accepted only after its semantic revision identity is recomputed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScopedRuntimeObserverEvent {
    pub event_id: String,
    pub semantic: FactSemanticRevision,
    pub operation: ScopedRuntimeOperation,
    pub retraction: Option<ScopedRuntimeRetraction>,
    pub revision: Fact,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MergedRuntimeContribution {
    pub semantic: FactSemanticRevision,
    pub revision: Fact,
    pub origin: MergedContributionOrigin,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DurableLiveRuntimeMerge {
    pub family: RuntimeSemanticFamily,
    pub contributions: Vec<MergedRuntimeContribution>,
    pub replacement_state_digest: [u8; 32],
    pub overlay: OverlayDisposition,
    pub delivered_observer_occurrences: Vec<DeliveredObserverOccurrence>,
}

struct RuntimeOverlayReduction {
    current: BTreeMap<CanonicalFactId, (FactSemanticRevision, Fact)>,
    retracted: BTreeSet<CanonicalFactId>,
    overlay_order: Vec<CanonicalFactId>,
    delivered: Vec<DeliveredObserverOccurrence>,
}

fn runtime_coverage_family(
    coverage: &SourceCoverageSet,
) -> Result<RuntimeSemanticFamily, SemanticMergeError> {
    coverage.validate()?;
    let CoverageDomain::FactFamily { family, version } = &coverage.coverage_domain else {
        return Err(SemanticMergeError::invalid(
            "runtime semantic merge requires fact-family coverage",
        ));
    };
    if *version != RuntimeSemanticFamily::VERSION {
        return Err(SemanticMergeError::invalid(
            "runtime semantic merge requires the selected family version",
        ));
    }
    RuntimeSemanticFamily::from_str(family).ok_or_else(|| {
        SemanticMergeError::invalid("runtime semantic merge coverage names an unsupported family")
    })
}

fn validate_runtime_contribution(
    family: RuntimeSemanticFamily,
    contribution: &DurableRuntimeContribution,
) -> Result<(), SemanticMergeError> {
    let actual = validate_runtime_fact_revision(&contribution.semantic, &contribution.revision)
        .map_err(|_| SemanticMergeError::invalid("runtime contribution is invalid"))?;
    if actual != family {
        return Err(SemanticMergeError::invalid(
            "runtime contribution does not match its coverage family",
        ));
    }
    if runtime_fact_declares_retraction(&contribution.revision)
        .map_err(|_| SemanticMergeError::invalid("runtime contribution is invalid"))?
        == Some(true)
    {
        return Err(SemanticMergeError::invalid(
            "durable current state cannot contain a retraction revision",
        ));
    }
    Ok(())
}

fn validate_runtime_event(
    family: RuntimeSemanticFamily,
    event: &ScopedRuntimeObserverEvent,
) -> Result<(), SemanticMergeError> {
    if event.event_id.is_empty() {
        return Err(SemanticMergeError::invalid(
            "observer event_id must be a non-empty occurrence identity",
        ));
    }
    let actual = validate_runtime_fact_revision(&event.semantic, &event.revision)
        .map_err(|_| SemanticMergeError::invalid("runtime observer event is invalid"))?;
    if actual != family {
        return Err(SemanticMergeError::invalid(
            "runtime observer event does not match its coverage family",
        ));
    }
    let operation_is_retract = event.operation == ScopedRuntimeOperation::Retract;
    if runtime_fact_declares_retraction(&event.revision)
        .map_err(|_| SemanticMergeError::invalid("runtime observer event is invalid"))?
        .is_some_and(|declared| declared != operation_is_retract)
    {
        return Err(SemanticMergeError::invalid(
            "runtime observer operation does not match its typed revision",
        ));
    }
    match (event.operation, event.retraction) {
        (ScopedRuntimeOperation::Upsert, Some(_)) => Err(SemanticMergeError::invalid(
            "runtime upsert cannot carry retraction metadata",
        )),
        (
            ScopedRuntimeOperation::Retract,
            Some(ScopedRuntimeRetraction::Reset {
                old_generation,
                new_generation,
            }),
        ) if old_generation == 0 || new_generation <= old_generation => {
            Err(SemanticMergeError::invalid(
                "runtime reset retraction requires an increasing nonzero generation",
            ))
        }
        (
            ScopedRuntimeOperation::Retract,
            Some(ScopedRuntimeRetraction::SourceDeleted { generation: 0 }),
        ) => Err(SemanticMergeError::invalid(
            "runtime deletion retraction requires a nonzero generation",
        )),
        _ => Ok(()),
    }
}

fn is_runtime_generation_retraction(event: &ScopedRuntimeObserverEvent) -> bool {
    event.operation == ScopedRuntimeOperation::Retract
        && matches!(
            event.retraction,
            Some(
                ScopedRuntimeRetraction::Reset { .. }
                    | ScopedRuntimeRetraction::SourceDeleted { .. }
            )
        )
}

fn reduce_runtime_observer_events(
    family: RuntimeSemanticFamily,
    durable: &[DurableRuntimeContribution],
    events: &[ScopedRuntimeObserverEvent],
) -> Result<RuntimeOverlayReduction, SemanticMergeError> {
    let mut seen_event_ids = BTreeMap::new();
    let mut unique = Vec::new();
    for event in events {
        validate_runtime_event(family, event)?;
        match seen_event_ids.get(event.event_id.as_str()) {
            Some(previous) if *previous != event => {
                return Err(SemanticMergeError::invalid(
                    "one observer event_id cannot identify different event content",
                ));
            }
            Some(_) => {}
            None => {
                seen_event_ids.insert(event.event_id.as_str(), event);
                unique.push(event);
            }
        }
    }

    let mut resets = Vec::new();
    let mut replay = Vec::new();
    for event in unique {
        if is_runtime_generation_retraction(event) {
            resets.push(event);
        } else {
            replay.push(event);
        }
    }

    let mut current = BTreeMap::new();
    let mut retracted = BTreeSet::new();
    let mut overlay_order = Vec::new();
    let mut delivered = Vec::new();
    let durable_by_fact_id = durable
        .iter()
        .map(|contribution| (contribution.semantic.fact_id, contribution))
        .collect::<BTreeMap<_, _>>();
    for event in resets.into_iter().chain(replay) {
        delivered.push(DeliveredObserverOccurrence {
            event_id: event.event_id.clone(),
            fact_id: event.semantic.fact_id,
            semantic_revision_ref: event.semantic.semantic_revision_ref,
        });
        let fact_id = event.semantic.fact_id;
        let topology_retraction = is_runtime_generation_retraction(event)
            || event.operation == ScopedRuntimeOperation::Retract
                && runtime_fact_declares_retraction(&event.revision)
                    .map_err(|_| SemanticMergeError::invalid("runtime observer event is invalid"))?
                    .is_none();
        let reduction = if topology_retraction {
            RuntimeFactReduction::Retract
        } else {
            let current_entity = if let Some((semantic, revision)) = current.get(&fact_id) {
                Some((semantic, revision))
            } else if retracted.contains(&fact_id) {
                None
            } else {
                durable_by_fact_id
                    .get(&fact_id)
                    .map(|contribution| (&contribution.semantic, &contribution.revision))
            };
            reduce_runtime_fact_revision(current_entity, (&event.semantic, &event.revision))
                .map_err(|_| {
                    SemanticMergeError::invalid(
                        "runtime observer transition violates its family reducer",
                    )
                })?
        };
        match reduction {
            RuntimeFactReduction::Retract => {
                current.remove(&fact_id);
                retracted.insert(fact_id);
            }
            RuntimeFactReduction::Unchanged => {}
            RuntimeFactReduction::Upsert { semantic, revision } => {
                if semantic != event.semantic {
                    return Err(SemanticMergeError::invalid(
                        "runtime observer event must carry its post-reducer semantic revision",
                    ));
                }
                retracted.remove(&event.semantic.fact_id);
                if !current.contains_key(&event.semantic.fact_id) {
                    overlay_order.push(event.semantic.fact_id);
                }
                current.insert(event.semantic.fact_id, (semantic, *revision));
            }
        }
    }
    Ok(RuntimeOverlayReduction {
        current,
        retracted,
        overlay_order,
        delivered,
    })
}

fn assemble_runtime_contributions(
    durable: &[DurableRuntimeContribution],
    overlay: &RuntimeOverlayReduction,
    disposition: OverlayDisposition,
) -> Vec<MergedRuntimeContribution> {
    if disposition == OverlayDisposition::Retired {
        return durable
            .iter()
            .map(|contribution| MergedRuntimeContribution {
                semantic: contribution.semantic,
                revision: contribution.revision.clone(),
                origin: MergedContributionOrigin::Durable,
            })
            .collect();
    }

    let mut seen = BTreeSet::new();
    let mut contributions = Vec::new();
    for contribution in durable {
        let fact_id = contribution.semantic.fact_id;
        if overlay.retracted.contains(&fact_id) && !overlay.current.contains_key(&fact_id) {
            continue;
        }
        if let Some((semantic, revision)) = overlay.current.get(&fact_id) {
            contributions.push(MergedRuntimeContribution {
                semantic: *semantic,
                revision: revision.clone(),
                origin: MergedContributionOrigin::Overlay,
            });
        } else {
            contributions.push(MergedRuntimeContribution {
                semantic: contribution.semantic,
                revision: contribution.revision.clone(),
                origin: MergedContributionOrigin::Durable,
            });
        }
        seen.insert(fact_id);
    }
    for fact_id in &overlay.overlay_order {
        if seen.contains(fact_id) {
            continue;
        }
        let Some((semantic, revision)) = overlay.current.get(fact_id) else {
            continue;
        };
        contributions.push(MergedRuntimeContribution {
            semantic: *semantic,
            revision: revision.clone(),
            origin: MergedContributionOrigin::Overlay,
        });
    }
    contributions
}

/// Reference downstream reconciliation for every closed RFC 012C fact family.
/// Inputs are already-typed common facts; native payloads have no representation
/// here. Coverage decides overlay retirement, `event_id` deduplicates delivery,
/// and canonical fact/revision identities decide replacement.
pub(crate) fn merge_durable_and_scoped_runtime(
    durable: &[DurableRuntimeContribution],
    durable_coverage: &SourceCoverageSet,
    observer_events: &[ScopedRuntimeObserverEvent],
    observer_coverage: &SourceCoverageSet,
) -> Result<DurableLiveRuntimeMerge, SemanticMergeError> {
    let family = runtime_coverage_family(durable_coverage)?;
    if runtime_coverage_family(observer_coverage)? != family {
        return Err(SemanticMergeError::invalid(
            "durable and observer coverage name different runtime families",
        ));
    }
    let mut durable_fact_ids = BTreeSet::new();
    for contribution in durable {
        if !durable_fact_ids.insert(contribution.semantic.fact_id) {
            return Err(SemanticMergeError::invalid(
                "durable runtime state contains a duplicate canonical fact identity",
            ));
        }
        validate_runtime_contribution(family, contribution)?;
    }
    let overlay = reduce_runtime_observer_events(family, durable, observer_events)?;
    let disposition = overlay_disposition(observer_coverage, durable_coverage)?;
    let contributions = assemble_runtime_contributions(durable, &overlay, disposition);
    let replacement_state_digest = runtime_replacement_state_digest(
        family,
        contributions
            .iter()
            .map(|contribution| RuntimeReplacementDigestEntity {
                semantic: &contribution.semantic,
                revision: &contribution.revision,
            }),
    )
    .map_err(|_| SemanticMergeError::invalid("merged runtime state is invalid"))?;
    Ok(DurableLiveRuntimeMerge {
        family,
        contributions,
        replacement_state_digest,
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
pub(crate) mod tests;
