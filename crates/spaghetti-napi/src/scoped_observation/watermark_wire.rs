//! Strict contextual RFC 012D poll watermark.
//!
//! A watermark reports one request-local completed poll's offered boundary. It
//! composes the exact capability, RFC 012A source coverage, declared scope
//! coverage, artifact availability, and queue state already retained by the
//! attachment. The context is non-Serde and can be minted only for a watermark
//! carrying the same process-local attachment authority.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{CoverageDomain, CoverageError, SourceCoverageSet};
use crate::observation_contract::{ObservationCapabilities, ObservationContractSelection};

use super::artifact_availability_wire::{
    ScopedArtifactAvailabilityConsumerContext, ScopedArtifactAvailabilityContextWire,
    ScopedArtifactAvailabilitySnapshotWire,
};
use super::capability_snapshot_wire::{
    ScopedCapabilitySnapshotConsumerContext, ScopedCapabilitySnapshotContextWire,
    ScopedCapabilitySnapshotWire,
};
use super::scope_coverage_wire::{
    ScopedScopeCoverageConsumerContext, ScopedScopeCoverageContextWire, ScopedScopeCoverageWire,
};
use super::unknown_evidence_wire::{
    UnknownEvidenceSnapshotConsumerContext, UnknownEvidenceSnapshotContextWire,
    UnknownEvidenceSnapshotWire,
};
use super::{
    ScopedObservationAttachmentAuthority, ScopedObservationContinuity,
    ScopedObservationDeliveryState, ScopedObservationRootIdentity, ScopedObservationWatermarkCore,
};

pub(crate) const SCOPED_OBSERVATION_WATERMARK_CONTRACT_VERSION: u32 = 2;

const MAX_SOURCE_COVERAGE_SETS: usize = 64;
const MAX_COVERAGE_ERRORS_PER_SET: usize = 4_096;
const MAX_EXPLICIT_OBJECT_ERRORS: usize = MAX_SOURCE_COVERAGE_SETS * MAX_COVERAGE_ERRORS_PER_SET;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedObservationWatermarkContractError {
    #[error("invalid scoped observation watermark contract: {message}")]
    Invalid { message: String },
    #[error("scoped observation watermark does not match caller-held context")]
    ContextMismatch,
}

impl ScopedObservationWatermarkContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }

    fn nested(error: impl std::fmt::Display) -> Self {
        Self::invalid(error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WatermarkContinuityWire {
    Bootstrap,
    Valid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatermarkQueueStateWire {
    scope_epoch: u64,
    offered_through_sequence: u64,
    delivered_through_sequence: u64,
    continuity: WatermarkContinuityWire,
    queued_semantic_events: u64,
    queued_retained_native_bytes: u64,
    queued_source_control_items: u64,
}

impl WatermarkQueueStateWire {
    fn from_internal(
        value: ScopedObservationDeliveryState,
    ) -> Result<Self, ScopedObservationWatermarkContractError> {
        let continuity = match value.continuity {
            ScopedObservationContinuity::Bootstrap => WatermarkContinuityWire::Bootstrap,
            ScopedObservationContinuity::Valid => WatermarkContinuityWire::Valid,
            ScopedObservationContinuity::ResyncRequired
            | ScopedObservationContinuity::Resyncing
            | ScopedObservationContinuity::Failed => {
                return Err(ScopedObservationWatermarkContractError::ContextMismatch)
            }
        };
        let wire = Self {
            scope_epoch: value.scope_epoch,
            offered_through_sequence: value.offered_through_sequence,
            delivered_through_sequence: value.delivered_through_sequence,
            continuity,
            queued_semantic_events: u64::try_from(value.queued_semantic_events)
                .map_err(|_| ScopedObservationWatermarkContractError::ContextMismatch)?,
            queued_retained_native_bytes: value.queued_retained_native_bytes,
            queued_source_control_items: u64::try_from(value.queued_source_control_items)
                .map_err(|_| ScopedObservationWatermarkContractError::ContextMismatch)?,
        };
        wire.validate()?;
        Ok(wire)
    }

    fn validate(&self) -> Result<(), ScopedObservationWatermarkContractError> {
        validate_positive_portable("watermark queue scope_epoch", self.scope_epoch)?;
        for (label, value) in [
            (
                "watermark queue offered_through_sequence",
                self.offered_through_sequence,
            ),
            (
                "watermark queue delivered_through_sequence",
                self.delivered_through_sequence,
            ),
            (
                "watermark queue semantic event count",
                self.queued_semantic_events,
            ),
            (
                "watermark queue retained-native byte count",
                self.queued_retained_native_bytes,
            ),
            (
                "watermark queue source-control item count",
                self.queued_source_control_items,
            ),
        ] {
            validate_nonnegative_portable(label, value)?;
        }
        let queued_items = self
            .queued_semantic_events
            .checked_add(self.queued_source_control_items)
            .ok_or_else(|| {
                ScopedObservationWatermarkContractError::invalid(
                    "watermark queue item count is exhausted",
                )
            })?;
        if self.delivered_through_sequence > self.offered_through_sequence
            || self
                .offered_through_sequence
                .checked_sub(self.delivered_through_sequence)
                != Some(queued_items)
        {
            return Err(ScopedObservationWatermarkContractError::invalid(
                "watermark queue counts do not match its offered boundary",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct WatermarkRootWire {
    session_ref: crate::adapter::ExternalEntityRef,
    session_key: crate::adapter::CanonicalEntityKey,
    root_actor_run_key: crate::adapter::CanonicalEntityKey,
    native_session_claim: Option<crate::adapter::NativeIdentityClaim>,
}

impl WatermarkRootWire {
    fn from_root(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            session_ref: root.session_ref,
            session_key: root.session_key,
            root_actor_run_key: root.root_actor_run_key,
            native_session_claim: root.native_session_claim.clone(),
        }
    }
}

/// Exact in-process context for one completed poll. Debug intentionally omits
/// root, support release, source coordinates, coverage, and semantic digests.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedObservationWatermarkConsumerContext {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    contract_selection: ObservationContractSelection,
    root: ScopedObservationRootIdentity,
    expected_scope_epoch: u64,
    expected_offered_through_sequence: u64,
    expected_source_coverage: Vec<SourceCoverageSet>,
    expected_explicit_object_errors: Vec<CoverageError>,
    expected_queue_state: WatermarkQueueStateWire,
    capability_context: Arc<ScopedCapabilitySnapshotConsumerContext>,
    scope_coverage_context: ScopedScopeCoverageConsumerContext,
    artifact_availability_context: ScopedArtifactAvailabilityConsumerContext,
    unknown_evidence_context: UnknownEvidenceSnapshotConsumerContext,
}

impl std::fmt::Debug for ScopedObservationWatermarkConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationWatermarkConsumerContext")
            .field("scope_epoch", &self.expected_scope_epoch)
            .field(
                "offered_through_sequence",
                &self.expected_offered_through_sequence,
            )
            .field(
                "source_coverage_set_count",
                &self.expected_source_coverage.len(),
            )
            .field(
                "explicit_object_error_count",
                &self.expected_explicit_object_errors.len(),
            )
            .finish_non_exhaustive()
    }
}

impl ScopedObservationWatermarkConsumerContext {
    pub(super) fn from_scoped_watermark(
        watermark: &ScopedObservationWatermarkCore,
        expected_attachment_authority: &Arc<ScopedObservationAttachmentAuthority>,
        capability_context: Arc<ScopedCapabilitySnapshotConsumerContext>,
    ) -> Result<Self, ScopedObservationWatermarkContractError> {
        if !Arc::ptr_eq(
            &watermark.attachment_authority,
            expected_attachment_authority,
        ) {
            return Err(ScopedObservationWatermarkContractError::ContextMismatch);
        }
        validate_watermark_shape(watermark, &capability_context)?;
        let scope_coverage_context = ScopedScopeCoverageConsumerContext::from_expected(
            &watermark.scope_coverage,
            &watermark.root,
            &watermark.source_coverage,
        )
        .map_err(ScopedObservationWatermarkContractError::nested)?;
        let artifact_availability_context =
            ScopedArtifactAvailabilityConsumerContext::from_expected(
                &watermark.observation_capabilities.selection,
                watermark.root.session_key,
                &watermark.artifact_availability,
            )
            .map_err(ScopedObservationWatermarkContractError::nested)?;
        let unknown_evidence_context =
            UnknownEvidenceSnapshotConsumerContext::from_expected(&watermark.unknown_evidence)
                .map_err(ScopedObservationWatermarkContractError::nested)?;
        let expected_queue_state = WatermarkQueueStateWire::from_internal(watermark.queue_state)?;
        let context = Self {
            attachment_authority: Arc::clone(expected_attachment_authority),
            contract_selection: watermark.observation_capabilities.selection.clone(),
            root: watermark.root.clone(),
            expected_scope_epoch: watermark.scope_epoch,
            expected_offered_through_sequence: watermark.offered_through_sequence,
            expected_source_coverage: watermark.source_coverage.clone(),
            expected_explicit_object_errors: watermark.explicit_object_errors.clone(),
            expected_queue_state,
            capability_context,
            scope_coverage_context,
            artifact_availability_context,
            unknown_evidence_context,
        };
        ScopedObservationWatermarkWire::from_scoped_for_context(watermark, &context)?;
        Ok(context)
    }

    pub(crate) fn wire(&self) -> ScopedObservationWatermarkContextWire {
        ScopedObservationWatermarkContextWire {
            contract_selection: self.contract_selection.clone(),
            adapter_id: self.root.adapter_id.as_str().to_owned(),
            root: WatermarkRootWire::from_root(&self.root),
            expected_scope_epoch: self.expected_scope_epoch,
            expected_offered_through_sequence: self.expected_offered_through_sequence,
            expected_source_coverage: self.expected_source_coverage.clone(),
            expected_explicit_object_errors: self.expected_explicit_object_errors.clone(),
            expected_queue_state: self.expected_queue_state,
            capability_context: self.capability_context.wire(),
            scope_coverage_context: self.scope_coverage_context.wire(),
            artifact_availability_context: self.artifact_availability_context.wire(),
            unknown_evidence_context: self.unknown_evidence_context.wire(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedObservationWatermarkContextWire {
    contract_selection: ObservationContractSelection,
    adapter_id: String,
    root: WatermarkRootWire,
    expected_scope_epoch: u64,
    expected_offered_through_sequence: u64,
    expected_source_coverage: Vec<SourceCoverageSet>,
    expected_explicit_object_errors: Vec<CoverageError>,
    expected_queue_state: WatermarkQueueStateWire,
    capability_context: ScopedCapabilitySnapshotContextWire,
    scope_coverage_context: ScopedScopeCoverageContextWire,
    artifact_availability_context: ScopedArtifactAvailabilityContextWire,
    unknown_evidence_context: UnknownEvidenceSnapshotContextWire,
}

/// Serialize-only poll watermark. Received JSON must be consumed with the
/// exact Rust-issued context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedObservationWatermarkWire {
    scoped_observation_watermark_contract_version: u32,
    contract_selection: ObservationContractSelection,
    root: WatermarkRootWire,
    scope_epoch: u64,
    offered_through_sequence: u64,
    source_coverage: Vec<SourceCoverageSet>,
    capability_snapshot: ScopedCapabilitySnapshotWire,
    scope_coverage: ScopedScopeCoverageWire,
    explicit_object_errors: Vec<CoverageError>,
    artifact_availability: ScopedArtifactAvailabilitySnapshotWire,
    unknown_evidence: UnknownEvidenceSnapshotWire,
    queue_state: WatermarkQueueStateWire,
}

/// One request-local poll completion after the owning attachment has rebound
/// the retained core watermark to its exact non-Serde consumer context. The
/// poll request generation remains solely in the ticket/runtime and never
/// enters this semantic result or either portable value.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedObservationCompletedPoll {
    watermark: Arc<ScopedObservationWatermarkCore>,
    context: ScopedObservationWatermarkConsumerContext,
    wire: ScopedObservationWatermarkWire,
}

impl std::fmt::Debug for ScopedObservationCompletedPoll {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedObservationCompletedPoll")
            .field("scope_epoch", &self.watermark.scope_epoch)
            .field(
                "offered_through_sequence",
                &self.watermark.offered_through_sequence,
            )
            .finish_non_exhaustive()
    }
}

impl ScopedObservationCompletedPoll {
    pub(super) fn from_resolved(
        watermark: Arc<ScopedObservationWatermarkCore>,
        context: ScopedObservationWatermarkConsumerContext,
    ) -> Result<Self, ScopedObservationWatermarkContractError> {
        let wire =
            ScopedObservationWatermarkWire::from_scoped_for_context(watermark.as_ref(), &context)?;
        Ok(Self {
            watermark,
            context,
            wire,
        })
    }

    pub(crate) fn watermark(&self) -> &Arc<ScopedObservationWatermarkCore> {
        &self.watermark
    }

    pub(crate) fn watermark_wire_value(
        &self,
    ) -> Result<JsonValue, ScopedObservationWatermarkContractError> {
        serde_json::to_value(&self.wire).map_err(ScopedObservationWatermarkContractError::nested)
    }

    pub(crate) fn context_wire_value(
        &self,
    ) -> Result<JsonValue, ScopedObservationWatermarkContractError> {
        serde_json::to_value(self.context.wire())
            .map_err(ScopedObservationWatermarkContractError::nested)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedObservationWatermarkInput {
    scoped_observation_watermark_contract_version: u32,
    contract_selection: JsonValue,
    root: JsonValue,
    scope_epoch: u64,
    offered_through_sequence: u64,
    source_coverage: JsonValue,
    capability_snapshot: JsonValue,
    scope_coverage: JsonValue,
    explicit_object_errors: JsonValue,
    artifact_availability: JsonValue,
    unknown_evidence: JsonValue,
    queue_state: WatermarkQueueStateWire,
}

impl ScopedObservationWatermarkWire {
    pub(crate) fn from_scoped_for_context(
        watermark: &ScopedObservationWatermarkCore,
        context: &ScopedObservationWatermarkConsumerContext,
    ) -> Result<Self, ScopedObservationWatermarkContractError> {
        if !Arc::ptr_eq(
            &watermark.attachment_authority,
            &context.attachment_authority,
        ) || watermark.root != context.root
            || watermark.scope_epoch != context.expected_scope_epoch
            || watermark.offered_through_sequence != context.expected_offered_through_sequence
            || watermark.source_coverage != context.expected_source_coverage
            || watermark.explicit_object_errors != context.expected_explicit_object_errors
            || WatermarkQueueStateWire::from_internal(watermark.queue_state)?
                != context.expected_queue_state
        {
            return Err(ScopedObservationWatermarkContractError::ContextMismatch);
        }
        let capability_snapshot =
            ScopedCapabilitySnapshotWire::from_capabilities(&watermark.observation_capabilities)
                .map_err(ScopedObservationWatermarkContractError::nested)?;
        let expected_capability =
            ScopedCapabilitySnapshotWire::from_context(&context.capability_context)
                .map_err(ScopedObservationWatermarkContractError::nested)?;
        if capability_snapshot != expected_capability {
            return Err(ScopedObservationWatermarkContractError::ContextMismatch);
        }
        let scope_coverage = ScopedScopeCoverageWire::from_expected(
            &watermark.scope_coverage,
            &context.scope_coverage_context,
        )
        .map_err(ScopedObservationWatermarkContractError::nested)?;
        let artifact_availability = ScopedArtifactAvailabilitySnapshotWire::from_context(
            &context.artifact_availability_context,
        )
        .map_err(ScopedObservationWatermarkContractError::nested)?;
        let actual_unknown_evidence_context =
            UnknownEvidenceSnapshotConsumerContext::from_expected(&watermark.unknown_evidence)
                .map_err(ScopedObservationWatermarkContractError::nested)?;
        if actual_unknown_evidence_context != context.unknown_evidence_context {
            return Err(ScopedObservationWatermarkContractError::ContextMismatch);
        }
        let unknown_evidence =
            UnknownEvidenceSnapshotWire::from_context(&context.unknown_evidence_context)
                .map_err(ScopedObservationWatermarkContractError::nested)?;
        Ok(Self {
            scoped_observation_watermark_contract_version:
                SCOPED_OBSERVATION_WATERMARK_CONTRACT_VERSION,
            contract_selection: context.contract_selection.clone(),
            root: WatermarkRootWire::from_root(&context.root),
            scope_epoch: context.expected_scope_epoch,
            offered_through_sequence: context.expected_offered_through_sequence,
            source_coverage: context.expected_source_coverage.clone(),
            capability_snapshot,
            scope_coverage,
            explicit_object_errors: context.expected_explicit_object_errors.clone(),
            artifact_availability,
            unknown_evidence,
            queue_state: context.expected_queue_state,
        })
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &ScopedObservationWatermarkConsumerContext,
    ) -> Result<Self, ScopedObservationWatermarkContractError> {
        preflight_arrays(&value, context)?;
        let input: ScopedObservationWatermarkInput = serde_json::from_value(value)
            .map_err(ScopedObservationWatermarkContractError::nested)?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            &context.contract_selection,
        )
        .map_err(ScopedObservationWatermarkContractError::nested)?;
        let root = serde_json::to_value(WatermarkRootWire::from_root(&context.root))
            .map_err(ScopedObservationWatermarkContractError::nested)?;
        let source_coverage = parse_source_coverage(input.source_coverage, context)?;
        let explicit_object_errors = parse_explicit_errors(input.explicit_object_errors, context)?;
        let capability_snapshot = ScopedCapabilitySnapshotWire::from_wire_value_for_context(
            input.capability_snapshot,
            &context.capability_context,
        )
        .map_err(ScopedObservationWatermarkContractError::nested)?;
        let scope_coverage = ScopedScopeCoverageWire::from_wire_value_for_context(
            input.scope_coverage,
            &context.scope_coverage_context,
        )
        .map_err(ScopedObservationWatermarkContractError::nested)?;
        let artifact_availability =
            ScopedArtifactAvailabilitySnapshotWire::from_wire_value_for_context(
                input.artifact_availability,
                &context.artifact_availability_context,
            )
            .map_err(ScopedObservationWatermarkContractError::nested)?;
        let unknown_evidence = UnknownEvidenceSnapshotWire::from_wire_value_for_context(
            input.unknown_evidence,
            &context.unknown_evidence_context,
        )
        .map_err(ScopedObservationWatermarkContractError::nested)?;
        input.queue_state.validate()?;
        if input.scoped_observation_watermark_contract_version
            != SCOPED_OBSERVATION_WATERMARK_CONTRACT_VERSION
            || contract_selection != context.contract_selection
            || input.root != root
            || input.scope_epoch != context.expected_scope_epoch
            || input.offered_through_sequence != context.expected_offered_through_sequence
            || input.queue_state != context.expected_queue_state
        {
            return Err(ScopedObservationWatermarkContractError::ContextMismatch);
        }
        Ok(Self {
            scoped_observation_watermark_contract_version: input
                .scoped_observation_watermark_contract_version,
            contract_selection,
            root: WatermarkRootWire::from_root(&context.root),
            scope_epoch: input.scope_epoch,
            offered_through_sequence: input.offered_through_sequence,
            source_coverage,
            capability_snapshot,
            scope_coverage,
            explicit_object_errors,
            artifact_availability,
            unknown_evidence,
            queue_state: input.queue_state,
        })
    }
}

fn validate_watermark_shape(
    watermark: &ScopedObservationWatermarkCore,
    capability_context: &ScopedCapabilitySnapshotConsumerContext,
) -> Result<(), ScopedObservationWatermarkContractError> {
    validate_positive_portable("watermark scope_epoch", watermark.scope_epoch)?;
    validate_nonnegative_portable(
        "watermark offered_through_sequence",
        watermark.offered_through_sequence,
    )?;
    if watermark.source_coverage.is_empty()
        || watermark.source_coverage.len() > MAX_SOURCE_COVERAGE_SETS
        || watermark
            .source_coverage
            .iter()
            .any(|coverage| coverage.explicit_errors.len() > MAX_COVERAGE_ERRORS_PER_SET)
        || !source_coverage_matches_authority(
            &watermark.root,
            capability_context,
            &watermark.source_coverage,
        )
        || !selected_family_coverage_is_complete(
            &watermark.observation_capabilities,
            &watermark.source_coverage,
        )
        || watermark.explicit_object_errors
            != canonical_explicit_errors(&watermark.source_coverage)?
        || WatermarkQueueStateWire::from_internal(watermark.queue_state)?.scope_epoch
            != watermark.scope_epoch
        || watermark.queue_state.offered_through_sequence != watermark.offered_through_sequence
    {
        return Err(ScopedObservationWatermarkContractError::ContextMismatch);
    }
    let actual =
        ScopedCapabilitySnapshotWire::from_capabilities(&watermark.observation_capabilities)
            .map_err(ScopedObservationWatermarkContractError::nested)?;
    let expected = ScopedCapabilitySnapshotWire::from_context(capability_context)
        .map_err(ScopedObservationWatermarkContractError::nested)?;
    if actual != expected {
        return Err(ScopedObservationWatermarkContractError::ContextMismatch);
    }
    Ok(())
}

fn source_coverage_matches_authority(
    root: &ScopedObservationRootIdentity,
    capability_context: &ScopedCapabilitySnapshotConsumerContext,
    source_coverage: &[SourceCoverageSet],
) -> bool {
    source_coverage.iter().all(|coverage| {
        coverage.scope.adapter_id == root.adapter_id.as_str()
            && coverage.scope.source_instance_key == root.source_instance_key
            && coverage.scope.root_entity_key == Some(root.session_key)
            && coverage.scope.support_release_id == capability_context.support_release_id()
    })
}

fn selected_family_coverage_is_complete(
    capabilities: &ObservationCapabilities,
    source_coverage: &[SourceCoverageSet],
) -> bool {
    if capabilities.validate().is_err() {
        return false;
    }
    let selected = &capabilities
        .selection
        .contract_versions
        .fact_family_versions;
    let mut observed = BTreeSet::new();
    let mut decode_count = 0usize;
    for coverage in source_coverage {
        if coverage.validate().is_err() {
            return false;
        }
        match &coverage.coverage_domain {
            CoverageDomain::Decode => decode_count += 1,
            CoverageDomain::FactFamily { family, version }
                if selected.get(family) == Some(version) =>
            {
                observed.insert((family.as_str(), *version));
            }
            CoverageDomain::FactFamily { .. } | CoverageDomain::ProjectionPack { .. } => {
                return false
            }
        }
    }
    selected.len().checked_add(1) == Some(source_coverage.len())
        && decode_count == 1
        && observed.len() == selected.len()
        && selected
            .iter()
            .all(|(family, version)| observed.contains(&(family.as_str(), *version)))
}

fn canonical_explicit_errors(
    source_coverage: &[SourceCoverageSet],
) -> Result<Vec<CoverageError>, ScopedObservationWatermarkContractError> {
    let combined = source_coverage.iter().try_fold(0usize, |count, coverage| {
        count
            .checked_add(coverage.explicit_errors.len())
            .ok_or_else(|| {
                ScopedObservationWatermarkContractError::invalid(
                    "watermark explicit error count is exhausted",
                )
            })
    })?;
    if combined > MAX_EXPLICIT_OBJECT_ERRORS {
        return Err(ScopedObservationWatermarkContractError::invalid(
            "watermark explicit object errors exceed the portable bound",
        ));
    }
    let mut errors = Vec::new();
    errors.try_reserve_exact(combined).map_err(|_| {
        ScopedObservationWatermarkContractError::invalid(
            "watermark explicit error allocation failed",
        )
    })?;
    for coverage in source_coverage {
        errors.extend(coverage.explicit_errors.iter().cloned());
    }
    errors.sort();
    errors.dedup();
    Ok(errors)
}

fn parse_source_coverage(
    value: JsonValue,
    context: &ScopedObservationWatermarkConsumerContext,
) -> Result<Vec<SourceCoverageSet>, ScopedObservationWatermarkContractError> {
    let values = value.as_array().ok_or_else(|| {
        ScopedObservationWatermarkContractError::invalid(
            "watermark source coverage must be an array",
        )
    })?;
    if values.len() != context.expected_source_coverage.len()
        || values.is_empty()
        || values.len() > MAX_SOURCE_COVERAGE_SETS
    {
        return Err(ScopedObservationWatermarkContractError::ContextMismatch);
    }
    let parsed = values
        .iter()
        .cloned()
        .map(|value| {
            serde_json::from_value::<SourceCoverageSet>(value)
                .map_err(ScopedObservationWatermarkContractError::nested)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parsed != context.expected_source_coverage {
        return Err(ScopedObservationWatermarkContractError::ContextMismatch);
    }
    Ok(parsed)
}

fn parse_explicit_errors(
    value: JsonValue,
    context: &ScopedObservationWatermarkConsumerContext,
) -> Result<Vec<CoverageError>, ScopedObservationWatermarkContractError> {
    let values = value.as_array().ok_or_else(|| {
        ScopedObservationWatermarkContractError::invalid(
            "watermark explicit object errors must be an array",
        )
    })?;
    if values.len() != context.expected_explicit_object_errors.len()
        || values.len() > MAX_EXPLICIT_OBJECT_ERRORS
    {
        return Err(ScopedObservationWatermarkContractError::ContextMismatch);
    }
    let parsed = values
        .iter()
        .cloned()
        .map(|value| {
            serde_json::from_value::<CoverageError>(value)
                .map_err(ScopedObservationWatermarkContractError::nested)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parsed != context.expected_explicit_object_errors {
        return Err(ScopedObservationWatermarkContractError::ContextMismatch);
    }
    Ok(parsed)
}

fn preflight_arrays(
    value: &JsonValue,
    context: &ScopedObservationWatermarkConsumerContext,
) -> Result<(), ScopedObservationWatermarkContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedObservationWatermarkContractError::invalid("watermark must be an object")
    })?;
    let fields = [
        "scoped_observation_watermark_contract_version",
        "contract_selection",
        "root",
        "scope_epoch",
        "offered_through_sequence",
        "source_coverage",
        "capability_snapshot",
        "scope_coverage",
        "explicit_object_errors",
        "artifact_availability",
        "unknown_evidence",
        "queue_state",
    ];
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !fields.contains(&field.as_str()))
    {
        return Err(ScopedObservationWatermarkContractError::invalid(
            "watermark fields do not match the exact contract",
        ));
    }
    let coverage_count = object
        .get("source_coverage")
        .and_then(JsonValue::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            ScopedObservationWatermarkContractError::invalid(
                "watermark source coverage must be an array",
            )
        })?;
    let error_count = object
        .get("explicit_object_errors")
        .and_then(JsonValue::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            ScopedObservationWatermarkContractError::invalid(
                "watermark explicit object errors must be an array",
            )
        })?;
    if coverage_count != context.expected_source_coverage.len()
        || coverage_count == 0
        || coverage_count > MAX_SOURCE_COVERAGE_SETS
        || error_count != context.expected_explicit_object_errors.len()
        || error_count > MAX_EXPLICIT_OBJECT_ERRORS
    {
        return Err(ScopedObservationWatermarkContractError::ContextMismatch);
    }
    Ok(())
}

fn validate_positive_portable(
    label: &str,
    value: u64,
) -> Result<(), ScopedObservationWatermarkContractError> {
    if value == 0 || value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedObservationWatermarkContractError::invalid(format!(
            "{label} must be a positive portable integer"
        )));
    }
    Ok(())
}

fn validate_nonnegative_portable(
    label: &str,
    value: u64,
) -> Result<(), ScopedObservationWatermarkContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedObservationWatermarkContractError::invalid(format!(
            "{label} must be a non-negative portable integer"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
