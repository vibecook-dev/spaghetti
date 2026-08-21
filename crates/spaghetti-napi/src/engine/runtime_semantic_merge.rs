//! Crate-private RFC 012C durable/scoped usage merge.
//!
//! Reconciles a durable usage-v2 contribution list with ordered scoped observer
//! upsert/retract events. Overlay retirement is coverage-gated. Consumers never
//! parse native JSON payloads.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::adapter::{
    compare_coverage, CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey,
    ContractCompleteness, CoverageComparison, CoverageSetCompleteness, FactRevisionId,
    SemanticContractError, SemanticRevisionRef, SourceCoverageSet, UsageRevisionV2Fact,
};
use crate::semantic_contract::MAX_SEMANTIC_FIXTURE_JSON_BYTES;

const USER_INPUT_FAMILY: &str = "runtime.user-input-request";
const USER_INPUT_FAMILY_VERSION: u32 = 1;
const INTERACTION_FIXTURE_CONTRACT_VERSION: u32 = 1;
const MAX_INTERACTION_QUESTIONS: usize = 32;
const MAX_INTERACTION_OPTIONS: usize = 32;
const MAX_INTERACTION_TEXT_BYTES: usize = 8 * 1024;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UserInputKind {
    Choice,
    MultiChoice,
    FreeText,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UserInputLifecycleState {
    Open,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UserInputOperation {
    Upsert,
    Retract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserInputOption {
    pub label: String,
    pub description: Option<String>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserInputQuestion {
    pub header: Option<String>,
    pub prompt: String,
    pub options: Vec<UserInputOption>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserInputLifecycleWire {
    state: UserInputLifecycleState,
    operation: UserInputOperation,
    completeness: ContractCompleteness,
    result_reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InteractionFixtureWire {
    fixture_contract_version: u32,
    runtime_semantic_contract_version: u32,
    family: String,
    family_version: u32,
    adapter_id: String,
    source_instance_key: CanonicalSourceInstanceKey,
    session: CanonicalEntityKey,
    actor_run: CanonicalEntityKey,
    native_tool_use_id: String,
    kind: UserInputKind,
    questions: Vec<UserInputQuestion>,
    open: UserInputLifecycleWire,
    resolved: UserInputLifecycleWire,
}

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
    pub family: &'static str,
    pub family_version: u32,
    pub open: UserInputLifecycleRevision,
    pub resolved: UserInputLifecycleRevision,
}

fn bounded_text(label: &str, value: &str) -> Result<(), SemanticMergeError> {
    if value.is_empty() || value.trim() != value {
        return Err(SemanticMergeError::invalid(format!(
            "{label} must be a non-empty canonical string"
        )));
    }
    if value.len() > MAX_INTERACTION_TEXT_BYTES {
        return Err(SemanticMergeError::invalid(format!(
            "{label} exceeds {MAX_INTERACTION_TEXT_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn validate_questions(questions: &[UserInputQuestion]) -> Result<(), SemanticMergeError> {
    if questions.is_empty() || questions.len() > MAX_INTERACTION_QUESTIONS {
        return Err(SemanticMergeError::invalid(format!(
            "interaction questions must contain 1..={MAX_INTERACTION_QUESTIONS} typed questions"
        )));
    }
    for question in questions {
        if let Some(header) = &question.header {
            bounded_text("question header", header)?;
        }
        bounded_text("question prompt", &question.prompt)?;
        if question.options.len() > MAX_INTERACTION_OPTIONS {
            return Err(SemanticMergeError::invalid(format!(
                "question options exceed {MAX_INTERACTION_OPTIONS}"
            )));
        }
        for option in &question.options {
            bounded_text("option label", &option.label)?;
            if let Some(description) = &option.description {
                bounded_text("option description", description)?;
            }
            if let Some(preview) = &option.preview {
                bounded_text("option preview", preview)?;
            }
        }
    }
    Ok(())
}

fn lifecycle_revision(
    wire: &InteractionFixtureWire,
    lifecycle: &UserInputLifecycleWire,
    expected_state: UserInputLifecycleState,
) -> Result<UserInputLifecycleRevision, SemanticMergeError> {
    if lifecycle.state != expected_state {
        return Err(SemanticMergeError::invalid(
            "interaction lifecycle state does not match its fixture slot",
        ));
    }
    if expected_state == UserInputLifecycleState::Resolved && lifecycle.result_reference.is_none() {
        return Err(SemanticMergeError::invalid(
            "resolved interaction requires a typed result_reference",
        ));
    }
    if expected_state == UserInputLifecycleState::Open && lifecycle.result_reference.is_some() {
        return Err(SemanticMergeError::invalid(
            "open interaction cannot carry a result_reference",
        ));
    }
    if let Some(result_reference) = &lifecycle.result_reference {
        bounded_text("result_reference", result_reference)?;
    }
    let fact_id = CanonicalFactId::native(
        &wire.adapter_id,
        &wire.source_instance_key,
        USER_INPUT_FAMILY,
        wire.native_tool_use_id.as_bytes(),
    )?;
    let revision_key = match expected_state {
        UserInputLifecycleState::Open => b"open".as_slice(),
        UserInputLifecycleState::Resolved => b"resolved".as_slice(),
    };
    Ok(UserInputLifecycleRevision {
        fact_id,
        semantic_revision_ref: SemanticRevisionRef::new(FactRevisionId::derive(
            &fact_id,
            1,
            revision_key,
        )?),
        session: wire.session,
        actor_run: wire.actor_run,
        native_tool_use_id: wire.native_tool_use_id.clone(),
        kind: wire.kind,
        questions: wire.questions.clone(),
        state: lifecycle.state,
        operation: lifecycle.operation,
        completeness: lifecycle.completeness,
        result_reference: lifecycle.result_reference.clone(),
    })
}

/// Parse the merge-test-only RFC 012C interaction lifecycle fixture.
pub(crate) fn parse_rfc012c_interaction_v1_json(
    json: &str,
) -> Result<InteractionContractFixture, SemanticMergeError> {
    if json.is_empty() {
        return Err(SemanticMergeError::invalid(
            "interaction fixture JSON must not be empty",
        ));
    }
    if json.len() > MAX_SEMANTIC_FIXTURE_JSON_BYTES {
        return Err(SemanticMergeError::invalid(format!(
            "interaction fixture JSON exceeds {MAX_SEMANTIC_FIXTURE_JSON_BYTES} bytes"
        )));
    }
    let wire: InteractionFixtureWire = serde_json::from_str(json)
        .map_err(|error| SemanticMergeError::invalid(error.to_string()))?;
    if wire.fixture_contract_version != INTERACTION_FIXTURE_CONTRACT_VERSION
        || wire.runtime_semantic_contract_version != INTERACTION_FIXTURE_CONTRACT_VERSION
    {
        return Err(SemanticMergeError::invalid(
            "unsupported interaction fixture contract version",
        ));
    }
    if wire.family != USER_INPUT_FAMILY || wire.family_version != USER_INPUT_FAMILY_VERSION {
        return Err(SemanticMergeError::invalid(
            "interaction fixture family must be runtime.user-input-request@1",
        ));
    }
    bounded_text("adapter_id", &wire.adapter_id)?;
    bounded_text("native_tool_use_id", &wire.native_tool_use_id)?;
    validate_questions(&wire.questions)?;
    let open = lifecycle_revision(&wire, &wire.open, UserInputLifecycleState::Open)?;
    let resolved = lifecycle_revision(&wire, &wire.resolved, UserInputLifecycleState::Resolved)?;
    if open.fact_id != resolved.fact_id {
        return Err(SemanticMergeError::invalid(
            "open and resolved interaction revisions must share one fact identity",
        ));
    }
    if open.semantic_revision_ref == resolved.semantic_revision_ref {
        return Err(SemanticMergeError::invalid(
            "open and resolved interaction revisions must have distinct semantic identity",
        ));
    }
    Ok(InteractionContractFixture {
        family: USER_INPUT_FAMILY,
        family_version: USER_INPUT_FAMILY_VERSION,
        open,
        resolved,
    })
}

#[cfg(test)]
mod tests;
