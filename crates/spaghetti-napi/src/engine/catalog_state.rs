//! Source-neutral RFC 012B Library plan and initial build-state durability.
//!
//! This module owns `Pending`/`Building`/`Partial` administration, reconstructs
//! the `Ready` snapshot published atomically by `catalog_publication`, and can
//! durably begin or recover a refresh while retaining that exact snapshot. An
//! active build may publish bounded partial milestones or fail with exact
//! integrity evidence; independently safe prior publications remain queryable.
//! It still owns no source reads, refresh completion/retirement policy, or
//! public query authority.

use std::io::{self, Write};
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::adapter::{
    CoverageDomain, CoverageMembershipRevision, CoverageSetCompleteness, SourceCoverageSet,
};
use crate::catalog_contract::evidence::{CatalogReducer, CatalogReducerPublication};
use crate::catalog_contract::publication::{
    CatalogPublicationMemberHistory, CatalogRefreshPredecessor,
};
use crate::catalog_contract::{
    validate_reason_code, CatalogCoveragePlan, CatalogCoveragePlanId, CatalogCoverageScope,
    CatalogIntegritySnapshotDisposition, CatalogReadinessMachine, CatalogReadinessPhase,
    CatalogReadinessReason, CatalogReadinessSnapshot, CatalogSnapshotId,
    CATALOG_PROJECTION_PACK_ID, CATALOG_QUERY_PACK_CONTRACT_VERSION,
    CATALOG_READINESS_CONTRACT_VERSION,
};

use super::catalog_publication::CatalogReadyPublicationIdentity;
use super::commit::{self, ChangeEntry};
use super::EngineError;

pub(super) const LIBRARY_SCOPE: &str = "library";
const PENDING_STATE: &str = "pending";
const BUILDING_STATE: &str = "building";
const PARTIAL_STATE: &str = "partial";
const READY_STATE: &str = "ready";
const DEGRADED_STATE: &str = "degraded";
const ERROR_STATE: &str = "error";
const REGISTER_REASON: &str = "catalog.library.plan.registered";
const SCHEDULE_REASON: &str = "catalog.library.build.scheduled";
pub(super) const PARTIAL_REASON: &str = "catalog.library.build.partial";
pub(super) const INITIAL_PUBLICATION_REASON: &str = "catalog.library.initial_snapshot.published";
const INITIAL_INTEGRITY_FAILURE_REASON: &str = "catalog.library.build.integrity_failed";
pub(super) const INITIAL_SOURCE_RETRYING_REASON: &str = "catalog.library.build.source_retrying";
const INITIAL_SOURCE_UNAVAILABLE_REASON: &str = "catalog.library.build.source_unavailable";
pub(super) const SOURCE_GENERATION_INVALIDATED_REASON: &str =
    "catalog.library.source_generation.invalidated";
const REFRESH_STARTED_REASON: &str = "catalog.library.refresh.started";
pub(super) const REFRESH_PUBLICATION_REASON: &str = "catalog.library.refresh_snapshot.published";
const REFRESH_INTEGRITY_FAILURE_REASON: &str = "catalog.library.refresh.integrity_failed";
pub(super) const REFRESH_SOURCE_RETRYING_REASON: &str = "catalog.library.refresh.source_retrying";
const REFRESH_SOURCE_UNAVAILABLE_REASON: &str = "catalog.library.refresh.source_unavailable";
pub(super) const REFRESH_RECOVERY_STARTED_REASON: &str = "catalog.library.refresh.recovery_started";
const READINESS_CHANGE_TOPIC: &str = "catalog.readiness.changed";
const READINESS_CHANGE_SCHEMA_VERSION: u32 = 1;
const REFRESH_CHANGE_SCHEMA_VERSION: u32 = 3;
const INTEGRITY_FAILURE_CHANGE_SCHEMA_VERSION: u32 = 5;
const SOURCE_UNAVAILABLE_CHANGE_SCHEMA_VERSION: u32 = 6;
const INITIAL_INTEGRITY_FAILURE_CHANGE_SCHEMA_VERSION: u32 = 7;
const PARTIAL_CHANGE_SCHEMA_VERSION: u32 = 8;
const EPOCH_INVALIDATION_CHANGE_SCHEMA_VERSION: u32 = 9;
const INITIAL_SOURCE_CHANGE_SCHEMA_VERSION: u32 = 10;
const MAX_CATALOG_PLAN_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_PARTIAL_SOURCE_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_PARTIAL_COVERAGE_BYTES: usize = 512 * 1024 * 1024;
const MAX_PARTIAL_SOURCES: usize = 4_096;
const MAX_PARTIAL_HISTORY: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogDurableBuildPhase {
    Pending,
    Building,
    Partial,
    Ready,
    Degraded,
    Error,
}

impl CatalogDurableBuildPhase {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => PENDING_STATE,
            Self::Building => BUILDING_STATE,
            Self::Partial => PARTIAL_STATE,
            Self::Ready => READY_STATE,
            Self::Degraded => DEGRADED_STATE,
            Self::Error => ERROR_STATE,
        }
    }

    fn parse(value: &str) -> Result<Self, EngineError> {
        match value {
            PENDING_STATE => Ok(Self::Pending),
            BUILDING_STATE => Ok(Self::Building),
            PARTIAL_STATE => Ok(Self::Partial),
            READY_STATE => Ok(Self::Ready),
            DEGRADED_STATE => Ok(Self::Degraded),
            ERROR_STATE => Ok(Self::Error),
            _ => Err(corrupt_catalog_state(format!(
                "unsupported durable build state {value:?}"
            ))),
        }
    }

    fn readiness_phase(self) -> CatalogReadinessPhase {
        match self {
            Self::Pending => CatalogReadinessPhase::Pending,
            Self::Building => CatalogReadinessPhase::Building,
            Self::Partial => CatalogReadinessPhase::Partial,
            Self::Ready => CatalogReadinessPhase::Ready,
            Self::Degraded => CatalogReadinessPhase::Degraded,
            Self::Error => CatalogReadinessPhase::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogBuildExpectation {
    pub scope: CatalogCoverageScope,
    pub coverage_plan_id: CatalogCoveragePlanId,
    pub desired_contract_version: u32,
    pub epoch: u64,
    pub attempt: u64,
    pub state: CatalogDurableBuildPhase,
    pub state_commit_seq: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogInitialBuildIntegrityExpectation {
    scope: CatalogCoverageScope,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogDurableBuildPhase,
    build_started_commit_seq: u64,
    retry_reason_code: Option<String>,
}

/// Non-transferable authority for one exact no-snapshot source pass. Unlike a
/// refresh expectation, this carries no retained publication or query
/// authority. The optional retry code is part of the compare-and-swap state so
/// a terminal transition cannot be replayed against a different retry.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogInitialBuildSourceExpectation {
    scope: CatalogCoverageScope,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogDurableBuildPhase,
    build_state_commit_seq: u64,
    retry_reason_code: Option<String>,
}

impl std::fmt::Debug for CatalogInitialBuildSourceExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogInitialBuildSourceExpectation")
            .field("scope", &self.scope)
            .field("coverage_plan_id", &self.coverage_plan_id)
            .field("desired_contract_version", &self.desired_contract_version)
            .field("epoch", &self.epoch)
            .field("attempt", &self.attempt)
            .field("state", &self.state)
            .field("build_state_commit_seq", &self.build_state_commit_seq)
            .field("retrying", &self.retry_reason_code.is_some())
            .finish()
    }
}

impl std::fmt::Debug for CatalogInitialBuildIntegrityExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogInitialBuildIntegrityExpectation")
            .field("scope", &self.scope)
            .field("coverage_plan_id", &self.coverage_plan_id)
            .field("desired_contract_version", &self.desired_contract_version)
            .field("epoch", &self.epoch)
            .field("attempt", &self.attempt)
            .field("state", &self.state)
            .field("build_started_commit_seq", &self.build_started_commit_seq)
            .field("source_retrying", &self.retry_reason_code.is_some())
            .finish()
    }
}

/// Non-transferable compare-and-swap proof for one durable Partial milestone.
/// The writer derives it from restart-validated state; callers cannot invent a
/// predecessor commit or move progress across plan/epoch/attempt lineages.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogPartialBuildExpectation {
    scope: CatalogCoverageScope,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogDurableBuildPhase,
    state_commit_seq: u64,
}

/// Non-transferable authority to invalidate one exact non-error build state
/// after its bound source generation changes.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogSourceGenerationInvalidationExpectation {
    scope: CatalogCoverageScope,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogDurableBuildPhase,
    state_commit_seq: u64,
    last_complete_snapshot: Option<CatalogSnapshotId>,
}

impl std::fmt::Debug for CatalogSourceGenerationInvalidationExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogSourceGenerationInvalidationExpectation")
            .field("scope", &self.scope)
            .field("coverage_plan_id", &self.coverage_plan_id)
            .field("desired_contract_version", &self.desired_contract_version)
            .field("epoch", &self.epoch)
            .field("attempt", &self.attempt)
            .field("state", &self.state)
            .field("state_commit_seq", &self.state_commit_seq)
            .field("last_complete_snapshot", &self.last_complete_snapshot)
            .finish()
    }
}

impl std::fmt::Debug for CatalogPartialBuildExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogPartialBuildExpectation")
            .field("scope", &self.scope)
            .field("coverage_plan_id", &self.coverage_plan_id)
            .field("desired_contract_version", &self.desired_contract_version)
            .field("epoch", &self.epoch)
            .field("attempt", &self.attempt)
            .field("state", &self.state)
            .field("state_commit_seq", &self.state_commit_seq)
            .finish()
    }
}

impl CatalogPartialBuildExpectation {
    pub(crate) fn state_commit_seq(&self) -> u64 {
        self.state_commit_seq
    }
}

/// Non-transferable compare-and-swap proof for one restart-validated plain
/// Ready publication. The authenticated publication identity is shared rather
/// than copied, and no field is constructible outside this module.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogReadyRefreshExpectation {
    scope: CatalogCoverageScope,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    snapshot_id: CatalogSnapshotId,
    state_commit_seq: u64,
    publication_identity: Arc<CatalogReadyPublicationIdentity>,
}

/// Non-transferable compare-and-swap proof for publishing the successor of
/// one active ordinary refresh. It retains the exact restart-validated prior
/// publication rather than allowing callers to provide independent digests.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogActiveRefreshPublicationExpectation {
    scope: CatalogCoverageScope,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    predecessor_snapshot: CatalogSnapshotId,
    refresh_started_commit_seq: u64,
    publication_identity: Arc<CatalogReadyPublicationIdentity>,
    lineage: CatalogRefreshExecutionLineage,
    retry_reason_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogRefreshExecutionLineage {
    ActiveReady,
    RecoveryBuilding,
    RecoveryPartial,
    EpochReplacementBuilding,
    EpochReplacementPartial,
}

impl std::fmt::Debug for CatalogActiveRefreshPublicationExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogActiveRefreshPublicationExpectation")
            .field("scope", &self.scope)
            .field("coverage_plan_id", &self.coverage_plan_id)
            .field("desired_contract_version", &self.desired_contract_version)
            .field("epoch", &self.epoch)
            .field("attempt", &self.attempt)
            .field("predecessor_snapshot", &self.predecessor_snapshot)
            .field(
                "refresh_started_commit_seq",
                &self.refresh_started_commit_seq,
            )
            .field("publication_identity", &self.publication_identity)
            .field("lineage", &self.lineage)
            .field("retry_reason_code", &self.retry_reason_code)
            .finish_non_exhaustive()
    }
}

impl CatalogActiveRefreshPublicationExpectation {
    pub(crate) fn predecessor(&self) -> Result<CatalogRefreshPredecessor, EngineError> {
        self.publication_identity
            .refresh_predecessor(self.predecessor_snapshot)
    }

    pub(crate) fn resume_reducer(&self) -> CatalogReducer {
        self.publication_identity.resume_reducer()
    }

    pub(crate) fn prior_reducer(&self) -> &CatalogReducerPublication {
        self.publication_identity.reducer()
    }

    pub(crate) fn prior_member_history(&self) -> &CatalogPublicationMemberHistory {
        self.publication_identity.member_history()
    }

    pub(crate) fn refresh_started_commit_seq(&self) -> u64 {
        self.refresh_started_commit_seq
    }

    pub(super) fn predecessor_snapshot(&self) -> CatalogSnapshotId {
        self.predecessor_snapshot
    }

    pub(super) fn publication_identity(&self) -> &Arc<CatalogReadyPublicationIdentity> {
        &self.publication_identity
    }

    pub(super) fn is_recovery(&self) -> bool {
        matches!(
            self.lineage,
            CatalogRefreshExecutionLineage::RecoveryBuilding
                | CatalogRefreshExecutionLineage::RecoveryPartial
                | CatalogRefreshExecutionLineage::EpochReplacementBuilding
                | CatalogRefreshExecutionLineage::EpochReplacementPartial
        )
    }

    pub(super) fn durable_state(&self) -> CatalogDurableBuildPhase {
        match self.lineage {
            CatalogRefreshExecutionLineage::ActiveReady => CatalogDurableBuildPhase::Ready,
            CatalogRefreshExecutionLineage::RecoveryBuilding
            | CatalogRefreshExecutionLineage::EpochReplacementBuilding => {
                CatalogDurableBuildPhase::Building
            }
            CatalogRefreshExecutionLineage::RecoveryPartial
            | CatalogRefreshExecutionLineage::EpochReplacementPartial => {
                CatalogDurableBuildPhase::Partial
            }
        }
    }

    pub(super) fn is_epoch_replacement(&self) -> bool {
        matches!(
            self.lineage,
            CatalogRefreshExecutionLineage::EpochReplacementBuilding
                | CatalogRefreshExecutionLineage::EpochReplacementPartial
        )
    }

    pub(super) fn retry_reason_code(&self) -> Option<&str> {
        self.retry_reason_code.as_deref()
    }
}

impl std::fmt::Debug for CatalogReadyRefreshExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogReadyRefreshExpectation")
            .field("scope", &self.scope)
            .field("coverage_plan_id", &self.coverage_plan_id)
            .field("desired_contract_version", &self.desired_contract_version)
            .field("epoch", &self.epoch)
            .field("attempt", &self.attempt)
            .field("snapshot_id", &self.snapshot_id)
            .field("state_commit_seq", &self.state_commit_seq)
            .field("publication_identity", &self.publication_identity)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogBuildStateCommand {
    Register {
        plan: CatalogCoveragePlan,
        desired_contract_version: u32,
        started_at: i64,
        committed_at: i64,
    },
    Schedule {
        expected: CatalogBuildExpectation,
        started_at: i64,
        committed_at: i64,
    },
    RecordPartial {
        expected: CatalogPartialBuildExpectation,
        source_coverage: Vec<SourceCoverageSet>,
        started_at: i64,
        committed_at: i64,
    },
    InvalidateSourceGeneration {
        expected: CatalogSourceGenerationInvalidationExpectation,
        started_at: i64,
        committed_at: i64,
    },
    BeginRefresh {
        expected: CatalogReadyRefreshExpectation,
        started_at: i64,
        committed_at: i64,
    },
    FailInitialBuildIntegrity {
        expected: CatalogInitialBuildIntegrityExpectation,
        reason_code: String,
        started_at: i64,
        committed_at: i64,
    },
    MarkInitialBuildSourceRetrying {
        expected: CatalogInitialBuildSourceExpectation,
        reason_code: String,
        started_at: i64,
        committed_at: i64,
    },
    DegradeInitialBuildSource {
        expected: CatalogInitialBuildSourceExpectation,
        reason_code: String,
        started_at: i64,
        committed_at: i64,
    },
    FailActiveRefreshIntegrity {
        expected: CatalogActiveRefreshPublicationExpectation,
        reason_code: String,
        started_at: i64,
        committed_at: i64,
    },
    DegradeActiveRefresh {
        expected: CatalogActiveRefreshPublicationExpectation,
        reason_code: String,
        started_at: i64,
        committed_at: i64,
    },
    MarkActiveRefreshRetrying {
        expected: CatalogActiveRefreshPublicationExpectation,
        reason_code: String,
        started_at: i64,
        committed_at: i64,
    },
    RetryTerminalRefresh {
        expected: CatalogBuildExpectation,
        started_at: i64,
        committed_at: i64,
    },
}

impl CatalogBuildStateCommand {
    pub(crate) fn register(
        plan: CatalogCoveragePlan,
        desired_contract_version: u32,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::Register {
            plan,
            desired_contract_version,
            started_at,
            committed_at,
        }
    }

    pub(crate) fn schedule(
        expected: CatalogBuildExpectation,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::Schedule {
            expected,
            started_at,
            committed_at,
        }
    }

    pub(crate) fn record_partial(
        expected: CatalogPartialBuildExpectation,
        source_coverage: Vec<SourceCoverageSet>,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::RecordPartial {
            expected,
            source_coverage,
            started_at,
            committed_at,
        }
    }

    pub(crate) fn invalidate_source_generation(
        expected: CatalogSourceGenerationInvalidationExpectation,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::InvalidateSourceGeneration {
            expected,
            started_at,
            committed_at,
        }
    }

    pub(crate) fn begin_refresh(
        expected: CatalogReadyRefreshExpectation,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::BeginRefresh {
            expected,
            started_at,
            committed_at,
        }
    }

    pub(crate) fn fail_active_refresh_integrity(
        expected: CatalogActiveRefreshPublicationExpectation,
        reason_code: impl Into<String>,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::FailActiveRefreshIntegrity {
            expected,
            reason_code: reason_code.into(),
            started_at,
            committed_at,
        }
    }

    pub(crate) fn fail_initial_build_integrity(
        expected: CatalogInitialBuildIntegrityExpectation,
        reason_code: impl Into<String>,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::FailInitialBuildIntegrity {
            expected,
            reason_code: reason_code.into(),
            started_at,
            committed_at,
        }
    }

    pub(crate) fn mark_initial_build_source_retrying(
        expected: CatalogInitialBuildSourceExpectation,
        reason_code: impl Into<String>,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::MarkInitialBuildSourceRetrying {
            expected,
            reason_code: reason_code.into(),
            started_at,
            committed_at,
        }
    }

    pub(crate) fn degrade_initial_build_source(
        expected: CatalogInitialBuildSourceExpectation,
        reason_code: impl Into<String>,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::DegradeInitialBuildSource {
            expected,
            reason_code: reason_code.into(),
            started_at,
            committed_at,
        }
    }

    pub(crate) fn degrade_active_refresh(
        expected: CatalogActiveRefreshPublicationExpectation,
        reason_code: impl Into<String>,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::DegradeActiveRefresh {
            expected,
            reason_code: reason_code.into(),
            started_at,
            committed_at,
        }
    }

    pub(crate) fn mark_active_refresh_retrying(
        expected: CatalogActiveRefreshPublicationExpectation,
        reason_code: impl Into<String>,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::MarkActiveRefreshRetrying {
            expected,
            reason_code: reason_code.into(),
            started_at,
            committed_at,
        }
    }

    pub(crate) fn retry_terminal_refresh(
        expected: CatalogBuildExpectation,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::RetryTerminalRefresh {
            expected,
            started_at,
            committed_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableCatalogBuildState {
    pub plan: CatalogCoveragePlan,
    pub readiness: CatalogReadinessSnapshot,
    pub last_commit_seq: u64,
    ready_publication_identity: Option<Arc<CatalogReadyPublicationIdentity>>,
    ready_publication_coverage: Option<Vec<SourceCoverageSet>>,
    ready_publication_attempt: Option<u64>,
    recovery_origin: Option<CatalogDurableBuildPhase>,
    epoch_replacement: bool,
    retired_snapshot_count: usize,
}

/// Caller-held authority for a restart-validated immutable Ready publication.
/// The current durable state may be Ready or an independently-safe integrity
/// Error; the embedded readiness always describes the exact published
/// snapshot. It is intentionally non-serializable and exposes no policy-view
/// choice.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct CatalogReadyReadAuthority {
    plan: CatalogCoveragePlan,
    readiness: CatalogReadinessSnapshot,
    snapshot_id: CatalogSnapshotId,
    publication_identity: Arc<CatalogReadyPublicationIdentity>,
}

impl DurableCatalogBuildState {
    pub(crate) fn expectation(&self) -> Result<CatalogBuildExpectation, EngineError> {
        Ok(CatalogBuildExpectation {
            scope: self.readiness.scope,
            coverage_plan_id: self.readiness.coverage_plan_id,
            desired_contract_version: self.readiness.desired_contract_version,
            epoch: self.readiness.epoch,
            attempt: self.readiness.attempt,
            state: durable_phase(self.readiness.state)?,
            state_commit_seq: self.last_commit_seq,
        })
    }

    pub(crate) fn initial_integrity_expectation(
        &self,
    ) -> Result<CatalogInitialBuildIntegrityExpectation, EngineError> {
        let retry_reason_code = match self.readiness.reason.as_ref() {
            None => None,
            Some(CatalogReadinessReason::SourceRetrying { code }) => Some(code.clone()),
            Some(_) => {
                return Err(EngineError::InvalidCommit(
                    "catalog initial integrity failure cannot replace terminal readiness"
                        .to_string(),
                ));
            }
        };
        if self.readiness.scope != CatalogCoverageScope::Library
            || !matches!(
                self.readiness.state,
                CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
            )
            || self.readiness.completed_contract_version.is_some()
            || self.readiness.complete_through_commit.is_some()
            || self.readiness.last_complete_snapshot.is_some()
            || self.readiness.refreshing_from_snapshot.is_some()
        {
            return Err(EngineError::InvalidCommit(
                "catalog initial integrity failure requires one exact no-snapshot active-build lineage"
                    .to_string(),
            ));
        }
        Ok(CatalogInitialBuildIntegrityExpectation {
            scope: self.readiness.scope,
            coverage_plan_id: self.readiness.coverage_plan_id,
            desired_contract_version: self.readiness.desired_contract_version,
            epoch: self.readiness.epoch,
            attempt: self.readiness.attempt,
            state: durable_phase(self.readiness.state)?,
            build_started_commit_seq: self.last_commit_seq,
            retry_reason_code,
        })
    }

    pub(crate) fn initial_source_expectation(
        &self,
    ) -> Result<CatalogInitialBuildSourceExpectation, EngineError> {
        let retry_reason_code = match self.readiness.reason.as_ref() {
            None => None,
            Some(CatalogReadinessReason::SourceRetrying { code }) => Some(code.clone()),
            Some(_) => {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source pass cannot use terminal readiness".to_string(),
                ));
            }
        };
        if self.readiness.scope != CatalogCoverageScope::Library
            || !matches!(
                self.readiness.state,
                CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
            )
            || self.readiness.completed_contract_version.is_some()
            || self.readiness.complete_through_commit.is_some()
            || self.readiness.last_complete_snapshot.is_some()
            || self.readiness.refreshing_from_snapshot.is_some()
        {
            return Err(EngineError::InvalidCommit(
                "catalog initial source pass requires one exact no-snapshot active-build lineage"
                    .to_string(),
            ));
        }
        Ok(CatalogInitialBuildSourceExpectation {
            scope: self.readiness.scope,
            coverage_plan_id: self.readiness.coverage_plan_id,
            desired_contract_version: self.readiness.desired_contract_version,
            epoch: self.readiness.epoch,
            attempt: self.readiness.attempt,
            state: durable_phase(self.readiness.state)?,
            build_state_commit_seq: self.last_commit_seq,
            retry_reason_code,
        })
    }

    pub(crate) fn partial_expectation(
        &self,
    ) -> Result<CatalogPartialBuildExpectation, EngineError> {
        if self.readiness.scope != CatalogCoverageScope::Library
            || !matches!(
                self.readiness.state,
                CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
            )
            || self.readiness.complete_through_commit.is_some()
            || self.readiness.refreshing_from_snapshot.is_some()
            || self.readiness.reason.is_some()
        {
            return Err(EngineError::InvalidCommit(
                "catalog partial progress requires one exact active build lineage".to_string(),
            ));
        }
        match (
            self.readiness.completed_contract_version,
            self.readiness.last_complete_snapshot,
        ) {
            (None, None) => {}
            (Some(completed), Some(snapshot))
                if completed == snapshot.pack_contract_version
                    && completed == self.readiness.desired_contract_version
                    && (self.readiness.attempt
                        > self
                            .ready_publication_attempt
                            .unwrap_or(self.readiness.attempt)
                        || (self.readiness.epoch > snapshot.readiness_epoch
                            && self.epoch_replacement)) => {}
            _ => {
                return Err(EngineError::InvalidCommit(
                    "catalog partial progress has an invalid retained-snapshot shape".to_string(),
                ));
            }
        }
        if self.readiness.state == CatalogReadinessPhase::Partial
            && self.readiness.source_coverage.is_empty()
        {
            return Err(EngineError::InvalidCommit(
                "catalog Partial readiness is missing durable source coverage".to_string(),
            ));
        }
        Ok(CatalogPartialBuildExpectation {
            scope: self.readiness.scope,
            coverage_plan_id: self.readiness.coverage_plan_id,
            desired_contract_version: self.readiness.desired_contract_version,
            epoch: self.readiness.epoch,
            attempt: self.readiness.attempt,
            state: durable_phase(self.readiness.state)?,
            state_commit_seq: self.last_commit_seq,
        })
    }

    pub(crate) fn source_generation_invalidation_expectation(
        &self,
    ) -> Result<CatalogSourceGenerationInvalidationExpectation, EngineError> {
        let state = durable_phase(self.readiness.state)?;
        if self.readiness.scope != CatalogCoverageScope::Library
            || state == CatalogDurableBuildPhase::Error
            || self.readiness.coverage_plan_id != self.plan.coverage_plan_id
            || self.last_commit_seq == 0
        {
            return Err(EngineError::InvalidCommit(
                "catalog source-generation invalidation requires one exact non-error Library lineage"
                    .to_string(),
            ));
        }
        Ok(CatalogSourceGenerationInvalidationExpectation {
            scope: self.readiness.scope,
            coverage_plan_id: self.readiness.coverage_plan_id,
            desired_contract_version: self.readiness.desired_contract_version,
            epoch: self.readiness.epoch,
            attempt: self.readiness.attempt,
            state,
            state_commit_seq: self.last_commit_seq,
            last_complete_snapshot: self.readiness.last_complete_snapshot,
        })
    }

    pub(super) fn refresh_build_readiness(&self) -> Result<CatalogReadinessSnapshot, EngineError> {
        self.refresh_publication_expectation()?;
        let mut readiness = self.readiness.clone();
        readiness.source_coverage = self.ready_publication_coverage.clone().ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog refresh is missing its retained publication coverage".to_string(),
            )
        })?;
        Ok(readiness)
    }

    pub(super) fn ready_read_authority(&self) -> Result<CatalogReadyReadAuthority, EngineError> {
        self.plan.validate().map_err(catalog_contract_error)?;
        let snapshot_id = self.readiness.last_complete_snapshot.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog retained-page reads require a completed snapshot".to_string(),
            )
        })?;
        let state_lineage_is_exact = match (
            self.readiness.state,
            self.readiness.refreshing_from_snapshot,
            self.readiness.reason.as_ref(),
        ) {
            (CatalogReadinessPhase::Ready, None, None) => {
                self.last_commit_seq == snapshot_id.complete_commit
            }
            (CatalogReadinessPhase::Ready, Some(refreshing), None)
            | (
                CatalogReadinessPhase::Ready,
                Some(refreshing),
                Some(CatalogReadinessReason::SourceRetrying { .. }),
            ) => refreshing == snapshot_id && self.last_commit_seq > snapshot_id.complete_commit,
            (
                CatalogReadinessPhase::Error,
                None,
                Some(CatalogReadinessReason::IntegrityFailure {
                    snapshot_disposition: CatalogIntegritySnapshotDisposition::IndependentlySafe,
                    ..
                }),
            ) => self.last_commit_seq > snapshot_id.complete_commit,
            (
                CatalogReadinessPhase::Degraded,
                None,
                Some(CatalogReadinessReason::TerminalSourceUnavailable { .. }),
            ) => self.last_commit_seq > snapshot_id.complete_commit,
            (CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial, None, None)
            | (
                CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial,
                None,
                Some(CatalogReadinessReason::SourceRetrying { .. }),
            ) => {
                self.readiness.complete_through_commit.is_none()
                    && ((self.readiness.epoch == snapshot_id.readiness_epoch
                        && self.readiness.attempt
                            > self
                                .ready_publication_attempt
                                .unwrap_or(self.readiness.attempt))
                        || (self.readiness.epoch > snapshot_id.readiness_epoch
                            && self.epoch_replacement))
                    && self.last_commit_seq > snapshot_id.complete_commit
            }
            _ => false,
        };
        let completion_lineage_is_exact = self.readiness.complete_through_commit
            == Some(snapshot_id.complete_commit)
            || (self.readiness.complete_through_commit.is_none()
                && matches!(
                    self.readiness.state,
                    CatalogReadinessPhase::Building
                        | CatalogReadinessPhase::Partial
                        | CatalogReadinessPhase::Degraded
                        | CatalogReadinessPhase::Error
                ));
        if self.plan.scope != CatalogCoverageScope::Library
            || self.readiness.coverage_plan_id != self.plan.coverage_plan_id
            || self.readiness.completed_contract_version != Some(snapshot_id.pack_contract_version)
            || !completion_lineage_is_exact
            || self.readiness.epoch < snapshot_id.readiness_epoch
            || !state_lineage_is_exact
        {
            return Err(EngineError::InvalidCommit(
                "catalog retained-page read authority is outside its exact safe publication lineage"
                    .to_string(),
            ));
        }
        // Page payloads describe the immutable publication, not the mutable
        // current reconciliation state. The durable state retains the refresh
        // marker; the read authority freezes the same Ready snapshot view that
        // was originally published.
        let mut published_readiness = self.readiness.clone();
        published_readiness.coverage_plan_id = snapshot_id.coverage_plan_id;
        published_readiness.desired_contract_version = snapshot_id.pack_contract_version;
        published_readiness.state = CatalogReadinessPhase::Ready;
        published_readiness.completed_contract_version = Some(snapshot_id.pack_contract_version);
        published_readiness.epoch = snapshot_id.readiness_epoch;
        published_readiness.attempt = self.ready_publication_attempt.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog retained-page read authority is missing its publication attempt"
                    .to_string(),
            )
        })?;
        published_readiness.complete_through_commit = Some(snapshot_id.complete_commit);
        published_readiness.refreshing_from_snapshot = None;
        published_readiness.source_coverage =
            self.ready_publication_coverage.clone().ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog retained-page read authority is missing its publication coverage"
                        .to_string(),
                )
            })?;
        published_readiness.reason = None;
        let published_readiness =
            CatalogReadinessMachine::resume(self.plan.clone(), published_readiness)
                .map_err(catalog_contract_error)?
                .snapshot()
                .clone();
        Ok(CatalogReadyReadAuthority {
            plan: self.plan.clone(),
            readiness: published_readiness,
            snapshot_id,
            publication_identity: self.ready_publication_identity.clone().ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog Ready state is missing its restart-validated publication identity"
                        .to_string(),
                )
            })?,
        })
    }

    pub(crate) fn refresh_expectation(
        &self,
    ) -> Result<CatalogReadyRefreshExpectation, EngineError> {
        if self.readiness.state != CatalogReadinessPhase::Ready
            || self.readiness.reason.is_some()
            || self.readiness.refreshing_from_snapshot.is_some()
        {
            return Err(EngineError::InvalidCommit(
                "catalog ordinary refresh requires one exact plain Ready lineage".to_string(),
            ));
        }
        let authority = self.ready_read_authority()?;
        if !authority.publication_identity.permits_refresh_successor() {
            return Err(EngineError::InvalidCommit(
                "catalog ordinary refresh would exceed the bounded retained lineage depth"
                    .to_string(),
            ));
        }
        Ok(CatalogReadyRefreshExpectation {
            scope: self.readiness.scope,
            coverage_plan_id: self.readiness.coverage_plan_id,
            desired_contract_version: self.readiness.desired_contract_version,
            epoch: self.readiness.epoch,
            attempt: self.readiness.attempt,
            snapshot_id: authority.snapshot_id,
            state_commit_seq: self.last_commit_seq,
            publication_identity: Arc::clone(&authority.publication_identity),
        })
    }

    pub(crate) fn refresh_publication_expectation(
        &self,
    ) -> Result<CatalogActiveRefreshPublicationExpectation, EngineError> {
        let predecessor_snapshot = self.readiness.last_complete_snapshot.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog refresh publication requires a retained predecessor".to_string(),
            )
        })?;
        let is_epoch_replacement = self.readiness.epoch > predecessor_snapshot.readiness_epoch;
        if is_epoch_replacement != self.epoch_replacement {
            return Err(EngineError::InvalidCommit(
                "catalog refresh publication epoch origin is not restart-authenticated".to_string(),
            ));
        }
        let retry_reason_code = match self.readiness.reason.as_ref() {
            Some(CatalogReadinessReason::SourceRetrying { code }) => Some(code.clone()),
            None => None,
            Some(_) => {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh publication has a terminal readiness reason".to_string(),
                ));
            }
        };
        let lineage = match self.readiness.state {
            CatalogReadinessPhase::Ready
                if self.readiness.refreshing_from_snapshot == Some(predecessor_snapshot)
                    && self.readiness.complete_through_commit
                        == Some(predecessor_snapshot.complete_commit)
                    && matches!(
                        self.readiness.reason.as_ref(),
                        None | Some(CatalogReadinessReason::SourceRetrying { .. })
                    ) =>
            {
                CatalogRefreshExecutionLineage::ActiveReady
            }
            CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
                if self.readiness.refreshing_from_snapshot.is_none()
                    && self.readiness.complete_through_commit.is_none()
                    && matches!(
                        self.readiness.reason.as_ref(),
                        None | Some(CatalogReadinessReason::SourceRetrying { .. })
                    )
                    && self.readiness.completed_contract_version
                        == Some(predecessor_snapshot.pack_contract_version)
                    && ((self.readiness.epoch == predecessor_snapshot.readiness_epoch
                        && self.readiness.attempt
                            > self
                                .ready_publication_attempt
                                .unwrap_or(self.readiness.attempt))
                        || (self.readiness.epoch > predecessor_snapshot.readiness_epoch
                            && self.epoch_replacement)) =>
            {
                if self.readiness.epoch > predecessor_snapshot.readiness_epoch {
                    if self.readiness.state == CatalogReadinessPhase::Partial {
                        CatalogRefreshExecutionLineage::EpochReplacementPartial
                    } else {
                        CatalogRefreshExecutionLineage::EpochReplacementBuilding
                    }
                } else if self.readiness.state == CatalogReadinessPhase::Partial {
                    CatalogRefreshExecutionLineage::RecoveryPartial
                } else {
                    CatalogRefreshExecutionLineage::RecoveryBuilding
                }
            }
            _ => {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh publication lineage is not active or recovering".to_string(),
                ));
            }
        };
        if self.last_commit_seq <= predecessor_snapshot.complete_commit {
            return Err(EngineError::InvalidCommit(
                "catalog refresh publication lineage is inconsistent".to_string(),
            ));
        }
        Ok(CatalogActiveRefreshPublicationExpectation {
            scope: self.readiness.scope,
            coverage_plan_id: self.readiness.coverage_plan_id,
            desired_contract_version: self.readiness.desired_contract_version,
            epoch: self.readiness.epoch,
            attempt: self.readiness.attempt,
            predecessor_snapshot,
            refresh_started_commit_seq: self.last_commit_seq,
            publication_identity: self.ready_publication_identity.clone().ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog active refresh is missing its predecessor publication identity"
                        .to_string(),
                )
            })?,
            lineage,
            retry_reason_code,
        })
    }

    pub(crate) fn snapshot_retirement_expectation(
        &self,
    ) -> Result<super::catalog_retention::CatalogSnapshotRetirementExpectation, EngineError> {
        if self.readiness.state != CatalogReadinessPhase::Ready
            || self.readiness.reason.is_some()
            || self.readiness.refreshing_from_snapshot.is_some()
        {
            return Err(EngineError::InvalidCommit(
                "catalog snapshot retirement requires a plain Ready lineage".to_string(),
            ));
        }
        let authority = self.ready_read_authority()?;
        let chain = authority.publication_identity.retained_chain();
        if self.retired_snapshot_count >= chain.len().saturating_sub(1) {
            return Err(EngineError::InvalidCommit(
                "catalog Ready lineage has no query-retained predecessor to retire".to_string(),
            ));
        }
        let target = chain[self.retired_snapshot_count];
        let successor = *chain.last().ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog Ready lineage is missing its current snapshot commitment".to_string(),
            )
        })?;
        super::catalog_retention::CatalogSnapshotRetirementExpectation::new(
            self.readiness.scope,
            self.plan.coverage_plan_id,
            authority.contract_selection().clone(),
            self.readiness.epoch,
            self.readiness.attempt,
            self.last_commit_seq,
            self.retired_snapshot_count,
            target,
            successor,
        )
    }
}

impl CatalogReadyReadAuthority {
    pub(super) fn plan(&self) -> &CatalogCoveragePlan {
        &self.plan
    }

    pub(super) fn readiness(&self) -> &CatalogReadinessSnapshot {
        &self.readiness
    }

    pub(super) fn snapshot_id(&self) -> CatalogSnapshotId {
        self.snapshot_id
    }

    pub(super) fn publication_identity(&self) -> &CatalogReadyPublicationIdentity {
        self.publication_identity.as_ref()
    }

    pub(super) fn resume_reducer(&self) -> CatalogReducer {
        self.publication_identity.resume_reducer()
    }

    pub(super) fn contract_selection(&self) -> &crate::adapter::ContractVersionSelection {
        self.publication_identity.contract_selection()
    }

    pub(super) fn retained_chain(
        &self,
    ) -> &[super::catalog_publication::CatalogRetainedSnapshotCommitment] {
        self.publication_identity.retained_chain()
    }

    pub(super) fn for_historical_snapshot(
        &self,
        connection: &Connection,
        snapshot_id: CatalogSnapshotId,
        retired_prefix_len: usize,
    ) -> Result<Self, EngineError> {
        if snapshot_id == self.snapshot_id {
            return Ok(self.clone());
        }
        let chain_index = self
            .publication_identity
            .retained_chain()
            .iter()
            .position(|commitment| commitment.snapshot_id() == snapshot_id)
            .ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog historical snapshot is outside the current bounded ancestry"
                        .to_string(),
                )
            })?;
        if chain_index < retired_prefix_len {
            return Err(EngineError::InvalidCommit(
                "catalog historical snapshot is durably retired from query service".to_string(),
            ));
        }
        let commitment = self.publication_identity.retained_chain()[chain_index];
        let historical_attempt: i64 = connection
            .query_row(
                "SELECT attempt FROM catalog_snapshots WHERE snapshot_commit_seq = ?1",
                [to_i64(
                    snapshot_id.complete_commit,
                    "catalog historical snapshot commit",
                )?],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_error("load catalog historical snapshot attempt", error))?;
        let historical_attempt =
            positive_u64(historical_attempt, "catalog historical snapshot attempt")?;
        let loaded = super::catalog_publication::load_ready_publication(
            connection,
            &self.plan,
            snapshot_id,
            historical_attempt,
        )?;
        if !loaded.identity.matches_snapshot_commitment(commitment) {
            return Err(corrupt_catalog_state(
                "historical catalog publication differs from the current restart-authenticated ancestry",
            ));
        }
        let mut historical_readiness = self.readiness.clone();
        historical_readiness.coverage_plan_id = snapshot_id.coverage_plan_id;
        historical_readiness.desired_contract_version = snapshot_id.pack_contract_version;
        historical_readiness.completed_contract_version = Some(snapshot_id.pack_contract_version);
        historical_readiness.epoch = snapshot_id.readiness_epoch;
        historical_readiness.attempt = historical_attempt;
        historical_readiness.state = CatalogReadinessPhase::Ready;
        historical_readiness.complete_through_commit = Some(snapshot_id.complete_commit);
        historical_readiness.last_complete_snapshot = Some(snapshot_id);
        historical_readiness.refreshing_from_snapshot = None;
        historical_readiness.source_coverage = loaded.source_coverage;
        historical_readiness.reason = None;
        let machine = CatalogReadinessMachine::resume(self.plan.clone(), historical_readiness)
            .map_err(catalog_contract_error)?;
        Ok(Self {
            plan: self.plan.clone(),
            readiness: machine.snapshot().clone(),
            snapshot_id,
            publication_identity: Arc::new(loaded.identity),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogBuildStateReceipt {
    pub commit_seq: u64,
    pub readiness: CatalogReadinessSnapshot,
}

/// Deterministic transaction seams for the complete administrative write.
/// `AfterCommit` models a lost acknowledgement after SQLite durability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogCommitStage {
    BeforeTransaction,
    AfterCommitInsert,
    AfterPlanWrite,
    AfterPartialCoverageWrite,
    AfterEpochInvalidationWrite,
    AfterFailureEvidenceWrite,
    AfterBuildStateWrite,
    AfterOutboxInsert,
    BeforeCommit,
    AfterCommit,
}

pub(super) trait CatalogCommitHook {
    fn reach(&self, stage: CatalogCommitStage) -> Result<(), EngineError>;
}

struct NoopCatalogCommitHook;

impl CatalogCommitHook for NoopCatalogCommitHook {
    fn reach(&self, _stage: CatalogCommitStage) -> Result<(), EngineError> {
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogReadinessChangedPayload {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogReadinessPhase,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogPartialChangedPayload {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogReadinessPhase,
    source_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<String>,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogEpochInvalidatedPayload {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    previous_epoch: u64,
    previous_attempt: u64,
    epoch: u64,
    attempt: u64,
    previous_state: CatalogReadinessPhase,
    state: CatalogReadinessPhase,
    predecessor_state_commit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_complete_snapshot: Option<CatalogSnapshotId>,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogRefreshStartedPayload {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    completed_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogReadinessPhase,
    last_complete_snapshot: CatalogSnapshotId,
    refreshing_from_snapshot: CatalogSnapshotId,
    complete_through_commit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<String>,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogRefreshIntegrityFailurePayload<'a> {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    completed_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogReadinessPhase,
    last_complete_snapshot: CatalogSnapshotId,
    complete_through_commit: Option<u64>,
    reason_code: &'a str,
    snapshot_disposition: CatalogIntegritySnapshotDisposition,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogInitialIntegrityFailurePayload<'a> {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogReadinessPhase,
    reason_code: &'a str,
    snapshot_disposition: CatalogIntegritySnapshotDisposition,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogInitialSourceChangedPayload<'a> {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    previous_state: CatalogReadinessPhase,
    state: CatalogReadinessPhase,
    transition: &'static str,
    predecessor_state_commit: u64,
    source_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<&'a str>,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogRefreshSourceUnavailablePayload<'a> {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    completed_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogReadinessPhase,
    last_complete_snapshot: CatalogSnapshotId,
    complete_through_commit: Option<u64>,
    reason_code: &'a str,
    commit_seq: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogRefreshRecoveryStartedPayload {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: CatalogCoveragePlanId,
    desired_contract_version: u32,
    completed_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: CatalogReadinessPhase,
    last_complete_snapshot: CatalogSnapshotId,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<String>,
    commit_seq: u64,
}

#[derive(Clone, Copy)]
enum CatalogBuildStateWrite<'a> {
    InsertPlan,
    Schedule {
        expected: &'a CatalogBuildExpectation,
    },
    RecordPartial {
        expected: &'a CatalogPartialBuildExpectation,
        prepared: &'a PreparedCatalogPartialCoverage,
    },
    InvalidateSourceGeneration {
        expected: &'a CatalogSourceGenerationInvalidationExpectation,
    },
    BeginRefresh {
        expected: &'a CatalogReadyRefreshExpectation,
    },
    FailInitialBuildIntegrity {
        expected: &'a CatalogInitialBuildIntegrityExpectation,
        reason_code: &'a str,
    },
    MarkInitialBuildSourceRetrying {
        expected: &'a CatalogInitialBuildSourceExpectation,
        reason_code: &'a str,
    },
    DegradeInitialBuildSource {
        expected: &'a CatalogInitialBuildSourceExpectation,
        reason_code: &'a str,
    },
    FailActiveRefreshIntegrity {
        expected: &'a CatalogActiveRefreshPublicationExpectation,
        reason_code: &'a str,
    },
    DegradeActiveRefresh {
        expected: &'a CatalogActiveRefreshPublicationExpectation,
        reason_code: &'a str,
    },
    MarkActiveRefreshRetrying {
        expected: &'a CatalogActiveRefreshPublicationExpectation,
        reason_code: &'a str,
    },
    RetryTerminalRefresh {
        expected: &'a CatalogBuildExpectation,
    },
}

#[derive(Debug)]
struct PreparedCatalogPartialSource {
    adapter_id: String,
    source_instance_key: [u8; 32],
    payload: Vec<u8>,
    payload_digest: [u8; 32],
}

#[derive(Debug)]
struct PreparedCatalogPartialCoverage {
    sources: Vec<PreparedCatalogPartialSource>,
    encoded_bytes: usize,
    entries_digest: [u8; 32],
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("catalog partial coverage size overflow"))?;
        if next > self.limit {
            return Err(io::Error::other(
                "catalog partial source coverage exceeds its byte limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn visit_partial_coverage<F>(
    source_coverage: &[SourceCoverageSet],
    mut visit: F,
) -> Result<(usize, [u8; 32]), EngineError>
where
    F: FnMut(&SourceCoverageSet, Vec<u8>, [u8; 32]) -> Result<(), EngineError>,
{
    if source_coverage.is_empty() || source_coverage.len() > MAX_PARTIAL_SOURCES {
        return Err(EngineError::InvalidCommit(
            "catalog partial coverage source count is empty or unbounded".to_string(),
        ));
    }
    if !source_coverage.windows(2).all(|pair| {
        (&pair[0].scope.adapter_id, pair[0].scope.source_instance_key)
            < (&pair[1].scope.adapter_id, pair[1].scope.source_instance_key)
    }) {
        return Err(EngineError::InvalidCommit(
            "catalog partial coverage must be canonical and duplicate-free".to_string(),
        ));
    }

    let mut encoded_bytes = 0_usize;
    let mut entries_hasher = blake3::Hasher::new();
    entries_hasher.update(b"catalog-partial-coverage-entries-v1\0");
    for (ordinal, coverage) in source_coverage.iter().enumerate() {
        coverage.validate().map_err(|_| {
            EngineError::InvalidCommit("invalid catalog partial coverage".to_string())
        })?;
        let mut writer = BoundedJsonWriter::new(MAX_PARTIAL_SOURCE_PAYLOAD_BYTES);
        serde_json::to_writer(&mut writer, coverage).map_err(|_| {
            EngineError::InvalidCommit(
                "catalog partial source coverage exceeds its durable encoding bound".to_string(),
            )
        })?;
        if writer.bytes.is_empty() {
            return Err(EngineError::InvalidCommit(
                "catalog partial source coverage encoded to an empty payload".to_string(),
            ));
        }
        encoded_bytes = encoded_bytes
            .checked_add(writer.bytes.len())
            .ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog partial coverage aggregate byte count overflowed".to_string(),
                )
            })?;
        if encoded_bytes > MAX_PARTIAL_COVERAGE_BYTES {
            return Err(EngineError::InvalidCommit(
                "catalog partial coverage exceeds its aggregate byte bound".to_string(),
            ));
        }
        let payload_digest = *blake3::hash(&writer.bytes).as_bytes();
        let ordinal = u64::try_from(ordinal).map_err(|_| {
            EngineError::InvalidCommit("catalog partial source ordinal overflowed".to_string())
        })?;
        entries_hasher.update(&ordinal.to_be_bytes());
        entries_hasher.update(&(coverage.scope.adapter_id.len() as u64).to_be_bytes());
        entries_hasher.update(coverage.scope.adapter_id.as_bytes());
        entries_hasher.update(coverage.scope.source_instance_key.as_bytes());
        entries_hasher.update(&(writer.bytes.len() as u64).to_be_bytes());
        entries_hasher.update(&payload_digest);
        visit(coverage, writer.bytes, payload_digest)?;
    }
    Ok((encoded_bytes, *entries_hasher.finalize().as_bytes()))
}

fn prepare_partial_coverage(
    source_coverage: &[SourceCoverageSet],
) -> Result<PreparedCatalogPartialCoverage, EngineError> {
    let mut sources = Vec::with_capacity(source_coverage.len());
    let (encoded_bytes, entries_digest) =
        visit_partial_coverage(source_coverage, |coverage, payload, payload_digest| {
            sources.push(PreparedCatalogPartialSource {
                adapter_id: coverage.scope.adapter_id.clone(),
                source_instance_key: *coverage.scope.source_instance_key.as_bytes(),
                payload,
                payload_digest,
            });
            Ok(())
        })?;
    Ok(PreparedCatalogPartialCoverage {
        sources,
        encoded_bytes,
        entries_digest,
    })
}

fn partial_progress_strictly_advances(
    previous: &[SourceCoverageSet],
    next: &[SourceCoverageSet],
) -> bool {
    let mut changed = false;
    for prior in previous {
        let Some(current) = next.iter().find(|candidate| {
            candidate.scope.adapter_id == prior.scope.adapter_id
                && candidate.scope.source_instance_key == prior.scope.source_instance_key
        }) else {
            return false;
        };
        if current == prior {
            continue;
        }
        let rank = |value: CoverageSetCompleteness| match value {
            CoverageSetCompleteness::Unavailable => 0_u8,
            CoverageSetCompleteness::Partial => 1,
            CoverageSetCompleteness::Complete => 2,
        };
        if rank(current.completeness) <= rank(prior.completeness) {
            return false;
        }
        changed = true;
    }
    changed || next.len() > previous.len()
}

fn unavailable_source_coverage(
    coverage: &[SourceCoverageSet],
) -> Result<Vec<SourceCoverageSet>, EngineError> {
    let mut unavailable = coverage.to_vec();
    for set in &mut unavailable {
        set.completeness = CoverageSetCompleteness::Unavailable;
        set.validate()
            .map_err(|error| EngineError::InvalidCommit(error.to_string()))?;
    }
    Ok(unavailable)
}

fn unavailable_initial_source_coverage(
    plan: &CatalogCoveragePlan,
    desired_contract_version: u32,
    coverage: &[SourceCoverageSet],
) -> Result<Vec<SourceCoverageSet>, EngineError> {
    let mut unavailable = unavailable_source_coverage(coverage)?;
    for source in &plan.required_sources {
        if unavailable
            .iter()
            .any(|candidate| source.matches_coverage(candidate))
        {
            continue;
        }
        let mut membership = Vec::with_capacity(128);
        membership.extend_from_slice(b"spaghetti/rfc012b/initial-source-unavailable-v1\0");
        membership.extend_from_slice(plan.coverage_plan_id.storage_bytes());
        membership.extend_from_slice(&desired_contract_version.to_be_bytes());
        membership.extend_from_slice(&(source.adapter_id.len() as u64).to_be_bytes());
        membership.extend_from_slice(source.adapter_id.as_bytes());
        membership.extend_from_slice(source.source_instance_key.as_bytes());
        let membership_revision = CoverageMembershipRevision::derive(&membership)
            .map_err(|error| EngineError::InvalidCommit(error.to_string()))?;
        unavailable.push(
            SourceCoverageSet::new(
                CoverageDomain::ProjectionPack {
                    pack: CATALOG_PROJECTION_PACK_ID.to_string(),
                    version: desired_contract_version,
                },
                source.coverage_scope(plan.scope),
                membership_revision,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                CoverageSetCompleteness::Unavailable,
            )
            .map_err(|error| EngineError::InvalidCommit(error.to_string()))?,
        );
    }
    unavailable.sort_by(|left, right| {
        (&left.scope.adapter_id, left.scope.source_instance_key)
            .cmp(&(&right.scope.adapter_id, right.scope.source_instance_key))
    });
    Ok(unavailable)
}

fn retrying_source_coverage(
    coverage: &[SourceCoverageSet],
) -> Result<Vec<SourceCoverageSet>, EngineError> {
    let mut retrying = coverage.to_vec();
    for set in &mut retrying {
        if set.completeness == CoverageSetCompleteness::Complete {
            set.completeness = CoverageSetCompleteness::Partial;
        }
        set.validate()
            .map_err(|error| EngineError::InvalidCommit(error.to_string()))?;
    }
    Ok(retrying)
}

fn active_refresh_matches_expectation(
    current: &DurableCatalogBuildState,
    expected: &CatalogReadyRefreshExpectation,
) -> bool {
    current.plan.scope == expected.scope
        && current.plan.coverage_plan_id == expected.coverage_plan_id
        && current.readiness.scope == expected.scope
        && current.readiness.coverage_plan_id == expected.coverage_plan_id
        && current.readiness.desired_contract_version == expected.desired_contract_version
        && current.readiness.completed_contract_version == Some(expected.desired_contract_version)
        && current.readiness.epoch == expected.epoch
        && current.readiness.attempt == expected.attempt
        && current.readiness.complete_through_commit == Some(expected.snapshot_id.complete_commit)
        && current.readiness.last_complete_snapshot == Some(expected.snapshot_id)
        && current.readiness.refreshing_from_snapshot == Some(expected.snapshot_id)
        && current.last_commit_seq > expected.state_commit_seq
        && current
            .ready_publication_identity
            .as_ref()
            .is_some_and(|identity| identity == &expected.publication_identity)
}

fn refresh_execution_reason(expected: &CatalogActiveRefreshPublicationExpectation) -> &'static str {
    if expected.retry_reason_code().is_some() {
        REFRESH_SOURCE_RETRYING_REASON
    } else if expected.durable_state() == CatalogDurableBuildPhase::Partial {
        PARTIAL_REASON
    } else if expected.is_epoch_replacement() {
        SOURCE_GENERATION_INVALIDATED_REASON
    } else if expected.is_recovery() {
        REFRESH_RECOVERY_STARTED_REASON
    } else {
        REFRESH_STARTED_REASON
    }
}

fn active_refresh_epoch_lineage_matches(
    expected: &CatalogActiveRefreshPublicationExpectation,
) -> bool {
    expected.predecessor_snapshot.readiness_epoch <= expected.epoch
        && expected.is_epoch_replacement()
            == (expected.predecessor_snapshot.readiness_epoch < expected.epoch)
}

pub(super) fn apply_catalog_build_state_commit(
    connection: &mut Connection,
    command: &CatalogBuildStateCommand,
) -> Result<Option<CatalogBuildStateReceipt>, EngineError> {
    apply_catalog_build_state_commit_with_hook(connection, command, &NoopCatalogCommitHook)
}

pub(super) fn apply_catalog_build_state_commit_with_hook(
    connection: &mut Connection,
    command: &CatalogBuildStateCommand,
    hook: &dyn CatalogCommitHook,
) -> Result<Option<CatalogBuildStateReceipt>, EngineError> {
    let prepared_partial = match command {
        CatalogBuildStateCommand::RecordPartial {
            source_coverage, ..
        } => Some(prepare_partial_coverage(source_coverage)?),
        _ => None,
    };
    validate_command(command)?;
    hook.reach(CatalogCommitStage::BeforeTransaction)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin catalog build-state commit", error))?;
    let current = load_catalog_build_state(&transaction)?;

    let (mut machine, started_at, committed_at, reason, write) = match command {
        CatalogBuildStateCommand::Register {
            plan,
            desired_contract_version,
            started_at,
            committed_at,
        } => {
            if let Some(current) = current {
                if current.plan == *plan
                    && current.readiness.desired_contract_version == *desired_contract_version
                    && matches!(
                        current.readiness.state,
                        CatalogReadinessPhase::Pending
                            | CatalogReadinessPhase::Building
                            | CatalogReadinessPhase::Partial
                            | CatalogReadinessPhase::Ready
                            | CatalogReadinessPhase::Degraded
                            | CatalogReadinessPhase::Error
                    )
                {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog plan registration", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog Library plan registration conflicts with the current build lineage"
                        .to_string(),
                ));
            }
            (
                CatalogReadinessMachine::register(plan.clone(), *desired_contract_version)
                    .map_err(catalog_contract_error)?,
                *started_at,
                *committed_at,
                REGISTER_REASON,
                CatalogBuildStateWrite::InsertPlan,
            )
        }
        CatalogBuildStateCommand::Schedule {
            expected,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog Library build cannot be scheduled before plan registration"
                        .to_string(),
                ));
            };
            let actual = current.expectation()?;
            if expected.state != CatalogDurableBuildPhase::Pending {
                return Err(EngineError::InvalidCommit(
                    "catalog build scheduling requires a pending compare-and-swap state"
                        .to_string(),
                ));
            }
            if actual.state == CatalogDurableBuildPhase::Building {
                let already_applied = CatalogBuildExpectation {
                    state: CatalogDurableBuildPhase::Pending,
                    state_commit_seq: expected.state_commit_seq,
                    ..actual.clone()
                };
                if *expected == already_applied {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog build scheduling", error)
                    })?;
                    return Ok(None);
                }
            }
            if *expected != actual {
                return Err(EngineError::InvalidCommit(
                    "catalog build scheduling compare-and-swap expectation is stale or foreign"
                        .to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine.schedule_build().map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                SCHEDULE_REASON,
                CatalogBuildStateWrite::Schedule { expected },
            )
        }
        CatalogBuildStateCommand::RecordPartial {
            expected,
            source_coverage,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog partial progress requires durable active readiness".to_string(),
                ));
            };
            let prepared = prepared_partial.as_ref().ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog partial progress is missing its bounded encoding".to_string(),
                )
            })?;
            if current.readiness.state == CatalogReadinessPhase::Partial
                && exact_partial_progress_exists(
                    &transaction,
                    &current,
                    expected,
                    prepared,
                    *started_at,
                    *committed_at,
                )?
            {
                transaction.commit().map_err(|error| {
                    sqlite_error("finish unchanged catalog partial progress", error)
                })?;
                return Ok(None);
            }
            if current.partial_expectation()? != *expected {
                return Err(EngineError::InvalidCommit(
                    "catalog partial-progress expectation is stale or foreign".to_string(),
                ));
            }
            if !partial_progress_strictly_advances(
                &current.readiness.source_coverage,
                source_coverage,
            ) {
                return Err(EngineError::InvalidCommit(
                    "catalog partial progress must strictly advance source coverage".to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .record_partial(source_coverage.clone())
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                PARTIAL_REASON,
                CatalogBuildStateWrite::RecordPartial { expected, prepared },
            )
        }
        CatalogBuildStateCommand::InvalidateSourceGeneration {
            expected,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog source-generation invalidation requires durable readiness".to_string(),
                ));
            };
            if exact_source_generation_invalidation_exists(
                &transaction,
                &current,
                expected,
                *started_at,
                *committed_at,
            )? {
                transaction.commit().map_err(|error| {
                    sqlite_error(
                        "finish unchanged catalog source-generation invalidation",
                        error,
                    )
                })?;
                return Ok(None);
            }
            if current.source_generation_invalidation_expectation()? != *expected {
                return Err(EngineError::InvalidCommit(
                    "catalog source-generation invalidation expectation is stale or foreign"
                        .to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .invalidate_source_generation()
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                SOURCE_GENERATION_INVALIDATED_REASON,
                CatalogBuildStateWrite::InvalidateSourceGeneration { expected },
            )
        }
        CatalogBuildStateCommand::BeginRefresh {
            expected,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog ordinary refresh cannot begin before Ready publication".to_string(),
                ));
            };
            if current.readiness.refreshing_from_snapshot.is_some() {
                if active_refresh_matches_expectation(&current, expected) {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog refresh start", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog ordinary refresh conflicts with the active refresh lineage"
                        .to_string(),
                ));
            }
            if current.refresh_expectation()? != *expected {
                return Err(EngineError::InvalidCommit(
                    "catalog ordinary refresh compare-and-swap expectation is stale or foreign"
                        .to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine.begin_refresh().map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                REFRESH_STARTED_REASON,
                CatalogBuildStateWrite::BeginRefresh { expected },
            )
        }
        CatalogBuildStateCommand::FailInitialBuildIntegrity {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog initial integrity failure requires durable Building readiness"
                        .to_string(),
                ));
            };
            if current.readiness.state == CatalogReadinessPhase::Error
                && current.readiness.last_complete_snapshot.is_none()
            {
                if exact_initial_integrity_failure_exists(
                    &transaction,
                    &current,
                    expected,
                    reason_code,
                    *started_at,
                    *committed_at,
                )? {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged initial catalog integrity failure", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog initial integrity failure conflicts with durable failure evidence"
                        .to_string(),
                ));
            }
            if current.initial_integrity_expectation()? != *expected {
                return Err(EngineError::InvalidCommit(
                    "catalog initial integrity-failure expectation is stale or foreign".to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .fail_integrity(
                    reason_code.clone(),
                    CatalogIntegritySnapshotDisposition::Discarded,
                )
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                INITIAL_INTEGRITY_FAILURE_REASON,
                CatalogBuildStateWrite::FailInitialBuildIntegrity {
                    expected,
                    reason_code,
                },
            )
        }
        CatalogBuildStateCommand::MarkInitialBuildSourceRetrying {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source retry requires durable active readiness".to_string(),
                ));
            };
            if current.readiness.reason
                == Some(CatalogReadinessReason::SourceRetrying {
                    code: reason_code.clone(),
                })
            {
                if exact_initial_source_retry_exists(
                    &transaction,
                    &current,
                    expected,
                    reason_code,
                    *started_at,
                    *committed_at,
                )? {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged initial catalog source retry", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog initial source retry conflicts with durable retry evidence"
                        .to_string(),
                ));
            }
            if expected.retry_reason_code.is_some()
                || current.initial_source_expectation()? != *expected
            {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source-retry expectation is stale or foreign".to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .source_retrying(
                    reason_code.clone(),
                    retrying_source_coverage(&current.readiness.source_coverage)?,
                )
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                INITIAL_SOURCE_RETRYING_REASON,
                CatalogBuildStateWrite::MarkInitialBuildSourceRetrying {
                    expected,
                    reason_code,
                },
            )
        }
        CatalogBuildStateCommand::DegradeInitialBuildSource {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source failure requires durable active readiness".to_string(),
                ));
            };
            if current.readiness.state == CatalogReadinessPhase::Degraded
                && current.readiness.last_complete_snapshot.is_none()
            {
                if exact_initial_source_failure_exists(
                    &transaction,
                    &current,
                    expected,
                    reason_code,
                    *started_at,
                    *committed_at,
                )? {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged initial catalog source failure", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog initial source failure conflicts with durable failure evidence"
                        .to_string(),
                ));
            }
            if current.initial_source_expectation()? != *expected {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source-failure expectation is stale or foreign".to_string(),
                ));
            }
            let source_coverage = unavailable_initial_source_coverage(
                &current.plan,
                current.readiness.desired_contract_version,
                &current.readiness.source_coverage,
            )?;
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .source_terminally_unavailable(reason_code.clone(), source_coverage)
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                INITIAL_SOURCE_UNAVAILABLE_REASON,
                CatalogBuildStateWrite::DegradeInitialBuildSource {
                    expected,
                    reason_code,
                },
            )
        }
        CatalogBuildStateCommand::FailActiveRefreshIntegrity {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh integrity failure requires a durable active refresh"
                        .to_string(),
                ));
            };
            if current.readiness.state == CatalogReadinessPhase::Error {
                if exact_integrity_failure_exists(
                    &transaction,
                    &current,
                    expected,
                    reason_code,
                    *started_at,
                    *committed_at,
                )? {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog integrity failure", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog refresh integrity failure conflicts with durable failure evidence"
                        .to_string(),
                ));
            }
            if current.refresh_publication_expectation()? != *expected {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh integrity-failure expectation is stale or foreign".to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .fail_integrity(
                    reason_code.clone(),
                    CatalogIntegritySnapshotDisposition::IndependentlySafe,
                )
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                REFRESH_INTEGRITY_FAILURE_REASON,
                CatalogBuildStateWrite::FailActiveRefreshIntegrity {
                    expected,
                    reason_code,
                },
            )
        }
        CatalogBuildStateCommand::MarkActiveRefreshRetrying {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog source retry requires a durable active refresh".to_string(),
                ));
            };
            if current.readiness.reason
                == Some(CatalogReadinessReason::SourceRetrying {
                    code: reason_code.clone(),
                })
            {
                if exact_source_retry_exists(
                    &transaction,
                    &current,
                    expected,
                    reason_code,
                    *started_at,
                    *committed_at,
                )? {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog source retry", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog source retry conflicts with the active retry lineage".to_string(),
                ));
            }
            if current.refresh_publication_expectation()? != *expected
                || expected.retry_reason_code().is_some()
            {
                return Err(EngineError::InvalidCommit(
                    "catalog source-retry expectation is stale or foreign".to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            let coverage = if current.readiness.source_coverage.is_empty() {
                current
                    .ready_publication_coverage
                    .as_deref()
                    .unwrap_or_default()
            } else {
                &current.readiness.source_coverage
            };
            machine
                .source_retrying(reason_code.clone(), retrying_source_coverage(coverage)?)
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                REFRESH_SOURCE_RETRYING_REASON,
                CatalogBuildStateWrite::MarkActiveRefreshRetrying {
                    expected,
                    reason_code,
                },
            )
        }
        CatalogBuildStateCommand::DegradeActiveRefresh {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog source failure requires a durable active refresh".to_string(),
                ));
            };
            if current.readiness.state == CatalogReadinessPhase::Degraded {
                if exact_source_failure_exists(
                    &transaction,
                    &current,
                    expected,
                    reason_code,
                    *started_at,
                    *committed_at,
                )? {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog source failure", error)
                    })?;
                    return Ok(None);
                }
                return Err(EngineError::InvalidCommit(
                    "catalog source failure conflicts with durable failure evidence".to_string(),
                ));
            }
            if current.refresh_publication_expectation()? != *expected {
                return Err(EngineError::InvalidCommit(
                    "catalog source-failure expectation is stale or foreign".to_string(),
                ));
            }
            let coverage = if current.readiness.source_coverage.is_empty() {
                current
                    .ready_publication_coverage
                    .as_deref()
                    .unwrap_or_default()
            } else {
                &current.readiness.source_coverage
            };
            let source_coverage = unavailable_source_coverage(coverage)?;
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .source_terminally_unavailable(reason_code.clone(), source_coverage)
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                REFRESH_SOURCE_UNAVAILABLE_REASON,
                CatalogBuildStateWrite::DegradeActiveRefresh {
                    expected,
                    reason_code,
                },
            )
        }
        CatalogBuildStateCommand::RetryTerminalRefresh {
            expected,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery requires durable terminal readiness".to_string(),
                ));
            };
            let actual = current.expectation()?;
            if actual.state == CatalogDurableBuildPhase::Building {
                let already_applied = actual.attempt
                    == expected.attempt.checked_add(1).ok_or_else(|| {
                        EngineError::InvalidCommit("catalog recovery attempt overflow".to_string())
                    })?
                    && actual.scope == expected.scope
                    && actual.coverage_plan_id == expected.coverage_plan_id
                    && actual.desired_contract_version == expected.desired_contract_version
                    && actual.epoch == expected.epoch
                    && current.recovery_origin == Some(expected.state);
                if already_applied {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog recovery start", error)
                    })?;
                    return Ok(None);
                }
            }
            if !matches!(
                expected.state,
                CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Error
            ) || *expected != actual
            {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery compare-and-swap expectation is stale or foreign".to_string(),
                ));
            }
            let source_coverage = if current.readiness.last_complete_snapshot.is_none() {
                Vec::new()
            } else {
                let coverage = if current
                    .plan
                    .required_coverage_present(&current.readiness.source_coverage)
                {
                    &current.readiness.source_coverage
                } else {
                    current
                        .ready_publication_coverage
                        .as_deref()
                        .unwrap_or_default()
                };
                unavailable_source_coverage(coverage)?
            };
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine
                .retry(source_coverage)
                .map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                REFRESH_RECOVERY_STARTED_REASON,
                CatalogBuildStateWrite::RetryTerminalRefresh { expected },
            )
        }
    };

    let commit_seq = insert_administrative_commit(&transaction, reason, started_at, committed_at)?;
    hook.reach(CatalogCommitStage::AfterCommitInsert)?;
    if matches!(write, CatalogBuildStateWrite::InsertPlan) {
        insert_plan(&transaction, machine.plan(), commit_seq)?;
    }
    hook.reach(CatalogCommitStage::AfterPlanWrite)?;
    if let CatalogBuildStateWrite::RecordPartial { expected, prepared } = write {
        insert_partial_coverage(&transaction, commit_seq, expected, prepared, committed_at)?;
        hook.reach(CatalogCommitStage::AfterPartialCoverageWrite)?;
    }
    if let CatalogBuildStateWrite::InvalidateSourceGeneration { expected } = write {
        insert_source_generation_invalidation(&transaction, commit_seq, expected, committed_at)?;
        hook.reach(CatalogCommitStage::AfterEpochInvalidationWrite)?;
    }
    if let CatalogBuildStateWrite::FailInitialBuildIntegrity {
        expected,
        reason_code,
    } = write
    {
        insert_initial_integrity_failure_evidence(
            &transaction,
            commit_seq,
            expected,
            reason_code,
            committed_at,
        )?;
        hook.reach(CatalogCommitStage::AfterFailureEvidenceWrite)?;
    }
    if let CatalogBuildStateWrite::FailActiveRefreshIntegrity {
        expected,
        reason_code,
    } = write
    {
        insert_integrity_failure_evidence(
            &transaction,
            commit_seq,
            expected,
            reason_code,
            committed_at,
        )?;
        hook.reach(CatalogCommitStage::AfterFailureEvidenceWrite)?;
    }
    if let CatalogBuildStateWrite::DegradeInitialBuildSource {
        expected,
        reason_code,
    } = write
    {
        insert_initial_source_failure_evidence(
            &transaction,
            commit_seq,
            expected,
            reason_code,
            committed_at,
        )?;
        hook.reach(CatalogCommitStage::AfterFailureEvidenceWrite)?;
    }
    if let CatalogBuildStateWrite::DegradeActiveRefresh {
        expected,
        reason_code,
    } = write
    {
        insert_source_failure_evidence(
            &transaction,
            commit_seq,
            expected,
            reason_code,
            committed_at,
        )?;
        hook.reach(CatalogCommitStage::AfterFailureEvidenceWrite)?;
    }
    write_build_state(&transaction, &mut machine, commit_seq, committed_at, write)?;
    hook.reach(CatalogCommitStage::AfterBuildStateWrite)?;
    write_readiness_change(&transaction, commit_seq, machine.snapshot(), write)?;
    hook.reach(CatalogCommitStage::AfterOutboxInsert)?;
    hook.reach(CatalogCommitStage::BeforeCommit)?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit catalog build-state transition", error))?;
    hook.reach(CatalogCommitStage::AfterCommit)?;
    Ok(Some(CatalogBuildStateReceipt {
        commit_seq,
        readiness: machine.snapshot().clone(),
    }))
}

pub(super) fn load_catalog_build_state(
    connection: &Connection,
) -> Result<Option<DurableCatalogBuildState>, EngineError> {
    let row = connection
        .query_row(
            r#"
            SELECT CASE WHEN typeof(plan.coverage_plan_id) = 'blob'
                                  AND length(plan.coverage_plan_id) = 32
                        THEN plan.coverage_plan_id END,
                   plan.coverage_plan_contract_version,
                   CASE
                     WHEN length(plan.plan_json) BETWEEN 1 AND ?2
                     THEN plan.plan_json
                   END,
                   CASE WHEN typeof(plan.content_digest) = 'blob'
                                  AND length(plan.content_digest) = 32
                        THEN plan.content_digest END,
                   plan.created_commit_seq,
                   build.desired_contract_version,
                   build.epoch,
                   build.attempt,
                   CASE WHEN typeof(build.state) = 'text'
                                  AND build.state IN ('pending', 'building', 'partial', 'ready', 'degraded', 'error')
                        THEN build.state END,
                   build.completed_contract_version,
                   build.complete_through_commit,
                   build.last_complete_snapshot_commit,
                   build.refreshing_from_snapshot_commit,
                   CASE WHEN build.reason_code IS NULL THEN NULL
                        WHEN typeof(build.reason_code) = 'text'
                                  AND length(CAST(build.reason_code AS BLOB)) BETWEEN 1 AND 64
                                  AND length(build.reason_code) = length(CAST(build.reason_code AS BLOB))
                                  AND substr(build.reason_code, 1, 1) GLOB '[a-z]'
                                  AND build.reason_code NOT GLOB '*[^a-z0-9_]*'
                        THEN build.reason_code
                        ELSE '' END,
                   build.last_commit_seq,
                   build.updated_at,
                   plan_commit.source_instance_id,
                   CASE WHEN typeof(plan_commit.reason) = 'text'
                                  AND plan_commit.reason = 'catalog.library.plan.registered'
                        THEN plan_commit.reason END,
                   plan_commit.committed_at,
                   plan_commit.fact_count,
                   state_commit.source_instance_id,
                   CASE WHEN typeof(state_commit.reason) = 'text'
                                  AND state_commit.reason IN (
                                      'catalog.library.plan.registered',
                                      'catalog.library.build.scheduled',
                                      'catalog.library.build.partial',
                                      'catalog.library.source_generation.invalidated',
                                      'catalog.library.initial_snapshot.published',
                                      'catalog.library.build.integrity_failed',
                                      'catalog.library.build.source_retrying',
                                      'catalog.library.build.source_unavailable',
                                      'catalog.library.refresh.started',
                                      'catalog.library.refresh_snapshot.published',
                                      'catalog.library.refresh.integrity_failed',
                                      'catalog.library.refresh.source_retrying',
                                      'catalog.library.refresh.source_unavailable',
                                      'catalog.library.refresh.recovery_started'
                                  )
                        THEN state_commit.reason END,
                   state_commit.committed_at,
                   state_commit.fact_count
            FROM catalog_build_state AS build
            JOIN catalog_coverage_plans AS plan
              ON plan.coverage_plan_id = build.coverage_plan_id
            JOIN ingest_commits AS plan_commit
              ON plan_commit.commit_seq = plan.created_commit_seq
            JOIN ingest_commits AS state_commit
              ON state_commit.commit_seq = build.last_commit_seq
            WHERE build.scope_kind = ?1 AND plan.scope_kind = ?1
            "#,
            params![LIBRARY_SCOPE, MAX_CATALOG_PLAN_JSON_BYTES as i64],
            |row| {
                Ok(StoredCatalogBuildState {
                    coverage_plan_id: row.get(0)?,
                    coverage_plan_contract_version: row.get(1)?,
                    plan_json: row.get(2)?,
                    content_digest: row.get(3)?,
                    created_commit_seq: row.get(4)?,
                    desired_contract_version: row.get(5)?,
                    epoch: row.get(6)?,
                    attempt: row.get(7)?,
                    state: row.get(8)?,
                    completed_contract_version: row.get(9)?,
                    complete_through_commit: row.get(10)?,
                    last_complete_snapshot_commit: row.get(11)?,
                    refreshing_from_snapshot_commit: row.get(12)?,
                    reason_code: row.get(13)?,
                    last_commit_seq: row.get(14)?,
                    updated_at: row.get(15)?,
                    plan_commit_source: row.get(16)?,
                    plan_commit_reason: row.get(17)?,
                    plan_committed_at: row.get(18)?,
                    plan_fact_count: row.get(19)?,
                    state_commit_source: row.get(20)?,
                    state_commit_reason: row.get(21)?,
                    state_committed_at: row.get(22)?,
                    state_fact_count: row.get(23)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_error("load catalog build state", error))?;

    let plan_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM catalog_coverage_plans", [], |row| {
            row.get(0)
        })
        .map_err(|error| sqlite_error("count catalog coverage plans", error))?;
    let state_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM catalog_build_state", [], |row| {
            row.get(0)
        })
        .map_err(|error| sqlite_error("count catalog build states", error))?;
    match row {
        Some(row) if plan_count == 1 && state_count == 1 => {
            decode_stored_state(connection, row).map(Some)
        }
        Some(_) => Err(corrupt_catalog_state(
            "durable catalog state must contain exactly one joined Library plan and build row",
        )),
        None if plan_count == 0 && state_count == 0 => Ok(None),
        None => Err(corrupt_catalog_state(
            "catalog coverage plan and Library build state do not form one valid lineage",
        )),
    }
}

struct StoredCatalogBuildState {
    coverage_plan_id: Option<Vec<u8>>,
    coverage_plan_contract_version: i64,
    plan_json: Option<Vec<u8>>,
    content_digest: Option<Vec<u8>>,
    created_commit_seq: i64,
    desired_contract_version: i64,
    epoch: i64,
    attempt: i64,
    state: Option<String>,
    completed_contract_version: Option<i64>,
    complete_through_commit: Option<i64>,
    last_complete_snapshot_commit: Option<i64>,
    refreshing_from_snapshot_commit: Option<i64>,
    reason_code: Option<String>,
    last_commit_seq: i64,
    updated_at: i64,
    plan_commit_source: Option<i64>,
    plan_commit_reason: Option<String>,
    plan_committed_at: Option<i64>,
    plan_fact_count: i64,
    state_commit_source: Option<i64>,
    state_commit_reason: Option<String>,
    state_committed_at: Option<i64>,
    state_fact_count: i64,
}

struct StoredCatalogPartialHeader {
    partial_commit_seq: i64,
    predecessor_state_commit_seq: i64,
    coverage_plan_id: Option<Vec<u8>>,
    readiness_epoch: i64,
    attempt: i64,
    source_count: i64,
    encoded_bytes: i64,
    entries_digest: Option<Vec<u8>>,
    committed_at: i64,
    owner_source: Option<i64>,
    owner_reason: Option<String>,
    owner_started_at: i64,
    owner_committed_at: Option<i64>,
    owner_fact_count: i64,
}

struct StoredCatalogPartialBaseChange {
    topic: Option<String>,
    schema_version: i64,
    entity_key: Option<Vec<u8>>,
    operation: Option<String>,
    payload: Option<Vec<u8>>,
}

struct StoredCatalogEpochInvalidation {
    invalidation_commit_seq: i64,
    predecessor_state_commit_seq: i64,
    coverage_plan_id: Option<Vec<u8>>,
    previous_epoch: i64,
    epoch: i64,
    previous_attempt: i64,
    previous_state: Option<String>,
    retained_snapshot_commit_seq: Option<i64>,
    committed_at: i64,
    owner_source: Option<i64>,
    owner_reason: Option<String>,
    owner_started_at: i64,
    owner_committed_at: Option<i64>,
    owner_fact_count: i64,
}

#[derive(Clone, Copy)]
enum CatalogPartialCoverageTarget {
    Exact {
        epoch: u64,
        attempt: u64,
        commit: u64,
    },
    LatestBefore {
        epoch: u64,
        attempt: u64,
        commit: u64,
    },
}

impl CatalogPartialCoverageTarget {
    fn matches_terminal(self, epoch: u64, attempt: u64, commit: u64) -> bool {
        match self {
            Self::Exact {
                epoch: target_epoch,
                attempt: target_attempt,
                commit: target_commit,
            } => epoch == target_epoch && attempt == target_attempt && commit == target_commit,
            Self::LatestBefore {
                epoch: target_epoch,
                attempt: target_attempt,
                commit: target_commit,
            } => epoch == target_epoch && attempt == target_attempt && commit < target_commit,
        }
    }
}

fn validate_partial_terminal_anchor(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    epoch: u64,
    attempt: u64,
    last_partial: u64,
) -> Result<(), EngineError> {
    let anchored: i64 = connection
        .query_row(
            r#"
            SELECT
              EXISTS(SELECT 1 FROM catalog_snapshots
                     WHERE build_commit_seq = ?1
                       AND coverage_plan_id = ?2
                       AND readiness_epoch = ?3
                       AND attempt = ?4)
              OR EXISTS(SELECT 1 FROM catalog_refresh_integrity_failures
                        WHERE failed_refresh_commit_seq = ?1
                          AND coverage_plan_id = ?2
                          AND readiness_epoch = ?3
                          AND attempt = ?4
                          AND snapshot_disposition = 'discarded')
              OR EXISTS(SELECT 1 FROM catalog_refresh_source_failures
                        WHERE failed_refresh_commit_seq = ?1
                          AND coverage_plan_id = ?2
                          AND readiness_epoch = ?3
                          AND attempt = ?4)
              OR EXISTS(SELECT 1 FROM catalog_initial_source_failures
                        WHERE failed_build_commit_seq = ?1
                          AND coverage_plan_id = ?2
                          AND readiness_epoch = ?3
                          AND attempt = ?4)
              OR EXISTS(SELECT 1 FROM catalog_build_state
                        WHERE last_commit_seq = ?1
                          AND coverage_plan_id = ?2
                          AND epoch = ?3
                          AND attempt = ?4
                          AND state = 'partial')
              OR EXISTS(SELECT 1 FROM catalog_epoch_invalidations
                        WHERE predecessor_state_commit_seq = ?1
                          AND coverage_plan_id = ?2
                          AND previous_epoch = ?3
                          AND previous_attempt = ?4)
              OR EXISTS(
                SELECT 1
                FROM ingest_commits AS retry
                WHERE retry.commit_seq > ?1
                  AND retry.source_instance_id IS NULL
                  AND retry.reason = 'catalog.library.refresh.source_retrying'
                  AND retry.committed_at IS NOT NULL
                  AND retry.fact_count = 0
                  AND (
                    EXISTS(SELECT 1 FROM catalog_snapshots
                           WHERE build_commit_seq = retry.commit_seq
                             AND coverage_plan_id = ?2
                             AND readiness_epoch = ?3
                             AND attempt = ?4)
                    OR EXISTS(SELECT 1 FROM catalog_refresh_integrity_failures
                              WHERE failed_refresh_commit_seq = retry.commit_seq
                                AND coverage_plan_id = ?2
                                AND readiness_epoch = ?3
                                AND attempt = ?4)
                    OR EXISTS(SELECT 1 FROM catalog_refresh_source_failures
                              WHERE failed_refresh_commit_seq = retry.commit_seq
                                AND coverage_plan_id = ?2
                                AND readiness_epoch = ?3
                                AND attempt = ?4)
                    OR EXISTS(SELECT 1 FROM catalog_build_state
                              WHERE last_commit_seq = retry.commit_seq
                                AND coverage_plan_id = ?2
                                AND epoch = ?3
                                AND attempt = ?4
                                AND state = 'partial'
                                AND reason_code IS NOT NULL)
                  )
              )
              OR EXISTS(
                SELECT 1
                FROM ingest_commits AS retry
                WHERE retry.commit_seq > ?1
                  AND retry.source_instance_id IS NULL
                  AND retry.reason = 'catalog.library.build.source_retrying'
                  AND retry.committed_at IS NOT NULL
                  AND retry.fact_count = 0
                  AND (
                    EXISTS(SELECT 1 FROM catalog_snapshots
                           WHERE build_commit_seq = retry.commit_seq
                             AND coverage_plan_id = ?2
                             AND readiness_epoch = ?3
                             AND attempt = ?4)
                    OR EXISTS(SELECT 1 FROM catalog_initial_source_failures
                              WHERE failed_build_commit_seq = retry.commit_seq
                                AND coverage_plan_id = ?2
                                AND readiness_epoch = ?3
                                AND attempt = ?4)
                    OR EXISTS(SELECT 1 FROM catalog_build_state
                              WHERE last_commit_seq = retry.commit_seq
                                AND coverage_plan_id = ?2
                                AND epoch = ?3
                                AND attempt = ?4
                                AND state = 'partial'
                                AND reason_code IS NOT NULL)
                  )
              )
            "#,
            params![
                to_i64(last_partial, "catalog last partial commit")?,
                plan.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(epoch, "catalog partial epoch")?,
                to_i64(attempt, "catalog partial attempt")?,
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate catalog partial terminal owner", error))?;
    if anchored != 1 {
        return Err(corrupt_catalog_state(
            "catalog partial chain is orphaned from durable readiness",
        ));
    }
    Ok(())
}

fn load_and_validate_partial_history(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    target: Option<CatalogPartialCoverageTarget>,
) -> Result<Option<Vec<SourceCoverageSet>>, EngineError> {
    let history_limit = MAX_PARTIAL_HISTORY.checked_add(1).ok_or_else(|| {
        corrupt_catalog_state("catalog partial-history validation limit overflow")
    })?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT partial.partial_commit_seq,
                   partial.predecessor_state_commit_seq,
                   CASE WHEN typeof(partial.coverage_plan_id) = 'blob'
                                  AND length(partial.coverage_plan_id) = 32
                        THEN partial.coverage_plan_id END,
                   partial.readiness_epoch, partial.attempt,
                   partial.source_count, partial.encoded_bytes,
                   CASE WHEN typeof(partial.entries_digest) = 'blob'
                                  AND length(partial.entries_digest) = 32
                        THEN partial.entries_digest END,
                   partial.committed_at,
                   owner.source_instance_id,
                   CASE WHEN typeof(owner.reason) = 'text'
                                  AND owner.reason = 'catalog.library.build.partial'
                        THEN owner.reason END,
                   owner.started_at, owner.committed_at, owner.fact_count
            FROM catalog_partial_builds AS partial
            LEFT JOIN ingest_commits AS owner
              ON owner.commit_seq = partial.partial_commit_seq
            ORDER BY partial.partial_commit_seq
            LIMIT ?1
            "#,
        )
        .map_err(|error| sqlite_error("prepare catalog partial history", error))?;
    let rows = statement
        .query_map(
            [to_i64(
                history_limit as u64,
                "catalog partial-history limit",
            )?],
            |row| {
                Ok(StoredCatalogPartialHeader {
                    partial_commit_seq: row.get(0)?,
                    predecessor_state_commit_seq: row.get(1)?,
                    coverage_plan_id: row.get(2)?,
                    readiness_epoch: row.get(3)?,
                    attempt: row.get(4)?,
                    source_count: row.get(5)?,
                    encoded_bytes: row.get(6)?,
                    entries_digest: row.get(7)?,
                    committed_at: row.get(8)?,
                    owner_source: row.get(9)?,
                    owner_reason: row.get(10)?,
                    owner_started_at: row.get(11)?,
                    owner_committed_at: row.get(12)?,
                    owner_fact_count: row.get(13)?,
                })
            },
        )
        .map_err(|error| sqlite_error("load catalog partial history", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("decode catalog partial history", error))?;
    if rows.len() > MAX_PARTIAL_HISTORY {
        return Err(corrupt_catalog_state(
            "catalog partial history exceeds its bounded restart scan",
        ));
    }

    // Coverage payloads may be hundreds of MiB. Validate the immutable history
    // one lineage at a time and retain only the terminal milestone required to
    // reconstruct the current Partial state. The history-row limit bounds SQL
    // work; this streaming shape independently bounds decoded payload memory.
    let mut active_group: Option<(u64, u64, u64, Vec<SourceCoverageSet>)> = None;
    let mut target_coverage = None;
    for stored in rows {
        let partial_commit = positive_u64(stored.partial_commit_seq, "catalog partial commit")?;
        let predecessor = positive_u64(
            stored.predecessor_state_commit_seq,
            "catalog partial predecessor commit",
        )?;
        let epoch = positive_u64(stored.readiness_epoch, "catalog partial epoch")?;
        let attempt = positive_u64(stored.attempt, "catalog partial attempt")?;
        let source_count = usize::try_from(stored.source_count)
            .map_err(|_| corrupt_catalog_state("catalog partial source count is outside usize"))?;
        let encoded_bytes = usize::try_from(stored.encoded_bytes).map_err(|_| {
            corrupt_catalog_state("catalog partial encoded bytes are outside usize")
        })?;
        if stored.coverage_plan_id.as_deref()
            != Some(plan.coverage_plan_id.storage_bytes().as_slice())
            || source_count == 0
            || source_count > MAX_PARTIAL_SOURCES
            || encoded_bytes == 0
            || encoded_bytes > MAX_PARTIAL_COVERAGE_BYTES
            || stored.entries_digest.as_deref().map(<[u8]>::len) != Some(32)
            || predecessor >= partial_commit
            || stored.owner_source.is_some()
            || stored.owner_reason.as_deref() != Some(PARTIAL_REASON)
            || stored.owner_started_at > stored.committed_at
            || stored.owner_committed_at != Some(stored.committed_at)
            || stored.owner_fact_count != 0
        {
            return Err(corrupt_catalog_state(
                "catalog partial header differs from its bounded administrative owner",
            ));
        }

        let mut entry_statement = connection
            .prepare(
                r#"
                SELECT ordinal,
                       CASE WHEN typeof(adapter_id) = 'text'
                                      AND length(CAST(adapter_id AS BLOB)) BETWEEN 1 AND 128
                            THEN adapter_id END,
                       CASE WHEN typeof(canonical_source_instance_key) = 'blob'
                                      AND length(canonical_source_instance_key) = 32
                            THEN canonical_source_instance_key END,
                       CASE WHEN typeof(payload) = 'blob'
                                      AND length(payload) BETWEEN 1 AND ?2
                            THEN payload END,
                       CASE WHEN typeof(payload_digest) = 'blob'
                                      AND length(payload_digest) = 32
                            THEN payload_digest END
                FROM catalog_partial_sources
                WHERE partial_commit_seq = ?1
                ORDER BY ordinal
                LIMIT ?3
                "#,
            )
            .map_err(|error| sqlite_error("prepare catalog partial sources", error))?;
        let entry_limit = source_count.checked_add(1).ok_or_else(|| {
            corrupt_catalog_state("catalog partial source validation limit overflow")
        })?;
        let entries = entry_statement
            .query_map(
                params![
                    to_i64(partial_commit, "catalog partial commit")?,
                    MAX_PARTIAL_SOURCE_PAYLOAD_BYTES as i64,
                    to_i64(entry_limit as u64, "catalog partial source limit")?,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                },
            )
            .map_err(|error| sqlite_error("load catalog partial sources", error))?;
        let mut coverage = Vec::with_capacity(source_count);
        for entry in entries {
            let (ordinal, adapter_id, source_key, payload, payload_digest) =
                entry.map_err(|error| sqlite_error("decode catalog partial source", error))?;
            let expected_ordinal = coverage.len();
            let payload = payload.ok_or_else(|| {
                corrupt_catalog_state("catalog partial source payload exceeds its byte bound")
            })?;
            let payload_digest = payload_digest.ok_or_else(|| {
                corrupt_catalog_state("catalog partial source digest is malformed")
            })?;
            let adapter_id = adapter_id.ok_or_else(|| {
                corrupt_catalog_state("catalog partial source adapter is malformed")
            })?;
            let source_key = source_key.ok_or_else(|| {
                corrupt_catalog_state("catalog partial source identity is malformed")
            })?;
            if usize::try_from(ordinal).ok() != Some(expected_ordinal)
                || blake3::hash(&payload).as_bytes().as_slice() != payload_digest
            {
                return Err(corrupt_catalog_state(
                    "catalog partial source ordinal or payload digest is invalid",
                ));
            }
            let set: SourceCoverageSet = serde_json::from_slice(&payload)
                .map_err(|_| corrupt_catalog_state("catalog partial source payload is invalid"))?;
            set.validate()
                .map_err(|_| corrupt_catalog_state("catalog partial source coverage is invalid"))?;
            if set.scope.adapter_id != adapter_id
                || set.scope.source_instance_key.as_bytes().as_slice() != source_key
            {
                return Err(corrupt_catalog_state(
                    "catalog partial source coordinates differ from its payload",
                ));
            }
            coverage.push(set);
        }
        if coverage.len() != source_count {
            return Err(corrupt_catalog_state(
                "catalog partial source count differs from its header",
            ));
        }
        let (canonical_encoded_bytes, canonical_entries_digest) =
            visit_partial_coverage(&coverage, |_, _, _| Ok(())).map_err(|_| {
                corrupt_catalog_state("catalog partial coverage encoding is not canonical")
            })?;
        if canonical_encoded_bytes != encoded_bytes
            || stored.entries_digest.as_deref() != Some(canonical_entries_digest.as_slice())
        {
            return Err(corrupt_catalog_state(
                "catalog partial coverage commitment differs from its entries",
            ));
        }
        let same_group = active_group
            .as_ref()
            .is_some_and(|(prior_epoch, prior_attempt, _, _)| {
                (*prior_epoch, *prior_attempt) == (epoch, attempt)
            });
        if same_group {
            let (_, _, prior_commit, prior_coverage) = active_group
                .as_ref()
                .expect("same partial-history group exists");
            if predecessor != *prior_commit
                || !partial_progress_strictly_advances(prior_coverage, &coverage)
            {
                return Err(corrupt_catalog_state(
                    "catalog partial milestones do not form one strictly advancing chain",
                ));
            }
        } else {
            if let Some((prior_epoch, prior_attempt, prior_commit, prior_coverage)) =
                active_group.take()
            {
                if (epoch, attempt) <= (prior_epoch, prior_attempt) {
                    return Err(corrupt_catalog_state(
                        "catalog partial lineages are not strictly ordered",
                    ));
                }
                validate_partial_terminal_anchor(
                    connection,
                    plan,
                    prior_epoch,
                    prior_attempt,
                    prior_commit,
                )?;
                if target.is_some_and(|target| {
                    target.matches_terminal(prior_epoch, prior_attempt, prior_commit)
                }) {
                    target_coverage = Some(prior_coverage);
                }
            }
            let expected_reason = if attempt == 1 && epoch > 1 {
                SOURCE_GENERATION_INVALIDATED_REASON
            } else if attempt == 1 {
                SCHEDULE_REASON
            } else {
                REFRESH_RECOVERY_STARTED_REASON
            };
            let (source, reason, committed_at, fact_count): (
                Option<i64>,
                Option<String>,
                Option<i64>,
                i64,
            ) = connection
                .query_row(
                    r#"
                    SELECT source_instance_id,
                           CASE WHEN typeof(reason) = 'text' THEN reason END,
                           committed_at, fact_count
                    FROM ingest_commits WHERE commit_seq = ?1
                    "#,
                    [to_i64(predecessor, "catalog partial base commit")?],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(|error| sqlite_error("load catalog partial base owner", error))?;
            if source.is_some()
                || reason.as_deref() != Some(expected_reason)
                || committed_at.is_none()
                || fact_count != 0
            {
                return Err(corrupt_catalog_state(
                    "catalog partial chain has a foreign build owner",
                ));
            }
            validate_partial_base_change(
                connection,
                plan,
                epoch,
                attempt,
                predecessor,
                expected_reason,
            )?;
        }
        active_group = Some((epoch, attempt, partial_commit, coverage));
    }

    if let Some((epoch, attempt, last_partial, coverage)) = active_group {
        validate_partial_terminal_anchor(connection, plan, epoch, attempt, last_partial)?;
        if target.is_some_and(|target| target.matches_terminal(epoch, attempt, last_partial)) {
            target_coverage = Some(coverage);
        }
    }
    Ok(target_coverage)
}

fn load_snapshot_id_at_commit(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    commit: u64,
) -> Result<CatalogSnapshotId, EngineError> {
    let (pack_contract_version, coverage_plan_id, readiness_epoch): (i64, Option<Vec<u8>>, i64) =
        connection
            .query_row(
                r#"
            SELECT pack_contract_version,
                   CASE WHEN typeof(coverage_plan_id) = 'blob'
                                  AND length(coverage_plan_id) = 32
                        THEN coverage_plan_id END,
                   readiness_epoch
            FROM catalog_snapshots WHERE snapshot_commit_seq = ?1
            "#,
                [to_i64(commit, "catalog snapshot commit")?],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| sqlite_error("load catalog snapshot identity", error))?;
    let coverage_plan_id = coverage_plan_id.ok_or_else(|| {
        corrupt_catalog_state("catalog snapshot coverage-plan ID exceeds its fixed durable bound")
    })?;
    if coverage_plan_id.as_slice() != plan.coverage_plan_id.storage_bytes().as_slice() {
        return Err(corrupt_catalog_state(
            "catalog retained snapshot belongs to a different coverage plan",
        ));
    }
    CatalogSnapshotId::new(
        positive_u32(pack_contract_version, "catalog snapshot pack version")?,
        plan.coverage_plan_id,
        positive_u64(readiness_epoch, "catalog snapshot readiness epoch")?,
        commit,
    )
    .map_err(catalog_contract_error)
}

fn load_and_validate_epoch_invalidation(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    desired_contract_version: u32,
    epoch: u64,
    retained_snapshot: Option<CatalogSnapshotId>,
) -> Result<Option<u64>, EngineError> {
    let stored = connection
        .query_row(
            r#"
            SELECT invalidation.invalidation_commit_seq,
                   invalidation.predecessor_state_commit_seq,
                   CASE WHEN typeof(invalidation.coverage_plan_id) = 'blob'
                                  AND length(invalidation.coverage_plan_id) = 32
                        THEN invalidation.coverage_plan_id END,
                   invalidation.previous_epoch, invalidation.epoch,
                   invalidation.previous_attempt,
                   CASE WHEN typeof(invalidation.previous_state) = 'text'
                                  AND invalidation.previous_state IN (
                                      'pending', 'building', 'partial', 'ready', 'degraded'
                                  )
                        THEN invalidation.previous_state END,
                   invalidation.retained_snapshot_commit_seq,
                   invalidation.committed_at,
                   owner.source_instance_id,
                   CASE WHEN typeof(owner.reason) = 'text'
                                  AND owner.reason = 'catalog.library.source_generation.invalidated'
                        THEN owner.reason END,
                   owner.started_at, owner.committed_at, owner.fact_count
            FROM catalog_epoch_invalidations AS invalidation
            LEFT JOIN ingest_commits AS owner
              ON owner.commit_seq = invalidation.invalidation_commit_seq
            WHERE invalidation.coverage_plan_id = ?1
              AND invalidation.epoch = ?2
            "#,
            params![
                plan.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(epoch, "catalog readiness epoch")?,
            ],
            |row| {
                Ok(StoredCatalogEpochInvalidation {
                    invalidation_commit_seq: row.get(0)?,
                    predecessor_state_commit_seq: row.get(1)?,
                    coverage_plan_id: row.get(2)?,
                    previous_epoch: row.get(3)?,
                    epoch: row.get(4)?,
                    previous_attempt: row.get(5)?,
                    previous_state: row.get(6)?,
                    retained_snapshot_commit_seq: row.get(7)?,
                    committed_at: row.get(8)?,
                    owner_source: row.get(9)?,
                    owner_reason: row.get(10)?,
                    owner_started_at: row.get(11)?,
                    owner_committed_at: row.get(12)?,
                    owner_fact_count: row.get(13)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_error("load catalog epoch invalidation", error))?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let commit_seq = positive_u64(
        stored.invalidation_commit_seq,
        "catalog epoch invalidation commit",
    )?;
    let predecessor_state_commit_seq = positive_u64(
        stored.predecessor_state_commit_seq,
        "catalog epoch invalidation predecessor",
    )?;
    let previous_epoch = positive_u64(stored.previous_epoch, "catalog previous readiness epoch")?;
    let stored_epoch = positive_u64(stored.epoch, "catalog readiness epoch")?;
    let previous_attempt = positive_u64(
        stored.previous_attempt,
        "catalog previous readiness attempt",
    )?;
    let previous_state =
        CatalogDurableBuildPhase::parse(stored.previous_state.as_deref().ok_or_else(|| {
            corrupt_catalog_state("catalog epoch invalidation has an invalid prior state")
        })?)?;
    let retained_snapshot_commit = stored
        .retained_snapshot_commit_seq
        .map(|value| positive_u64(value, "catalog invalidation retained snapshot"))
        .transpose()?;
    if stored.coverage_plan_id.as_deref() != Some(plan.coverage_plan_id.storage_bytes().as_slice())
        || previous_state == CatalogDurableBuildPhase::Error
        || stored_epoch != epoch
        || previous_epoch.checked_add(1) != Some(epoch)
        || predecessor_state_commit_seq >= commit_seq
        || retained_snapshot_commit != retained_snapshot.map(|snapshot| snapshot.complete_commit)
        || retained_snapshot_commit.is_some_and(|snapshot| snapshot > predecessor_state_commit_seq)
        || stored.owner_source.is_some()
        || stored.owner_reason.as_deref() != Some(SOURCE_GENERATION_INVALIDATED_REASON)
        || stored.owner_started_at > stored.committed_at
        || stored.owner_committed_at != Some(stored.committed_at)
        || stored.owner_fact_count != 0
    {
        return Err(corrupt_catalog_state(
            "catalog epoch invalidation differs from its exact administrative owner",
        ));
    }

    let expected_payload = serde_json::to_vec(&CatalogEpochInvalidatedPayload {
        readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
        scope: LIBRARY_SCOPE,
        coverage_plan_id: plan.coverage_plan_id,
        desired_contract_version,
        previous_epoch,
        previous_attempt,
        epoch,
        attempt: 1,
        previous_state: previous_state.readiness_phase(),
        state: CatalogReadinessPhase::Building,
        predecessor_state_commit: predecessor_state_commit_seq,
        last_complete_snapshot: retained_snapshot,
        commit_seq,
    })
    .map_err(|_| corrupt_catalog_state("catalog epoch invalidation cannot be re-encoded"))?;
    let change_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM change_log WHERE commit_seq = ?1",
            [to_i64(commit_seq, "catalog epoch invalidation commit")?],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("count catalog epoch invalidation changes", error))?;
    let change_matches: i64 = connection
        .query_row(
            r#"
            SELECT EXISTS(
              SELECT 1 FROM change_log
              WHERE commit_seq = ?1
                AND ordinal = 0
                AND topic = ?2
                AND schema_version = ?3
                AND entity_key = ?4
                AND operation = 'upsert'
                AND payload = ?5
            )
            "#,
            params![
                to_i64(commit_seq, "catalog epoch invalidation commit")?,
                READINESS_CHANGE_TOPIC,
                i64::from(EPOCH_INVALIDATION_CHANGE_SCHEMA_VERSION),
                plan.coverage_plan_id.storage_bytes().as_slice(),
                expected_payload,
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate catalog epoch invalidation change", error))?;
    if change_count != 1 || change_matches != 1 {
        return Err(corrupt_catalog_state(
            "catalog epoch invalidation is missing its exact durable change",
        ));
    }
    Ok(Some(commit_seq))
}

fn validate_initial_snapshot_epoch_replacement(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    snapshot_id: CatalogSnapshotId,
    publication_attempt: u64,
    build_commit_seq: u64,
) -> Result<(), EngineError> {
    if snapshot_id.readiness_epoch == 1 {
        return Ok(());
    }
    let invalidation = load_and_validate_epoch_invalidation(
        connection,
        plan,
        snapshot_id.pack_contract_version,
        snapshot_id.readiness_epoch,
        None,
    )?
    .ok_or_else(|| {
        corrupt_catalog_state(
            "cold catalog publication is missing source-generation invalidation evidence",
        )
    })?;
    if invalidation > build_commit_seq {
        return Err(corrupt_catalog_state(
            "cold catalog publication precedes its source-generation invalidation",
        ));
    }
    if publication_attempt == 1 && invalidation != build_commit_seq {
        let exact_partial: i64 = connection
            .query_row(
                r#"
                SELECT EXISTS(
                  SELECT 1 FROM catalog_partial_builds
                  WHERE partial_commit_seq = ?1
                    AND coverage_plan_id = ?2
                    AND readiness_epoch = ?3
                    AND attempt = 1
                )
                "#,
                params![
                    to_i64(build_commit_seq, "catalog initial build commit")?,
                    plan.coverage_plan_id.storage_bytes().as_slice(),
                    to_i64(snapshot_id.readiness_epoch, "catalog readiness epoch")?,
                ],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_error("validate cold catalog publication base", error))?;
        if exact_partial != 1 {
            return Err(corrupt_catalog_state(
                "cold catalog publication is not anchored to its invalidation or partial chain",
            ));
        }
    }
    if publication_attempt > 1 && invalidation >= build_commit_seq {
        return Err(corrupt_catalog_state(
            "retried cold catalog publication does not descend from its invalidation",
        ));
    }
    Ok(())
}

fn validate_partial_base_change(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    epoch: u64,
    attempt: u64,
    base_commit: u64,
    expected_reason: &str,
) -> Result<(), EngineError> {
    if expected_reason == SOURCE_GENERATION_INVALIDATED_REASON {
        let retained_snapshot_commit: Option<i64> = connection
            .query_row(
                r#"
                SELECT retained_snapshot_commit_seq
                FROM catalog_epoch_invalidations
                WHERE coverage_plan_id = ?1 AND epoch = ?2
                "#,
                params![
                    plan.coverage_plan_id.storage_bytes().as_slice(),
                    to_i64(epoch, "catalog readiness epoch")?,
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| sqlite_error("load catalog partial epoch base", error))?
            .flatten();
        let retained_snapshot = retained_snapshot_commit
            .map(|commit| {
                positive_u64(commit, "catalog invalidation retained snapshot")
                    .and_then(|commit| load_snapshot_id_at_commit(connection, plan, commit))
            })
            .transpose()?;
        let evidence = load_and_validate_epoch_invalidation(
            connection,
            plan,
            CATALOG_QUERY_PACK_CONTRACT_VERSION,
            epoch,
            retained_snapshot,
        )?
        .ok_or_else(|| {
            corrupt_catalog_state("catalog partial chain is missing its epoch invalidation")
        })?;
        if attempt != 1 || evidence != base_commit {
            return Err(corrupt_catalog_state(
                "catalog partial epoch base differs from its exact invalidation",
            ));
        }
        return Ok(());
    }
    let change_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM change_log WHERE commit_seq = ?1",
            [to_i64(base_commit, "catalog partial base commit")?],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("count catalog partial base changes", error))?;
    let stored = connection
        .query_row(
            r#"
            SELECT CASE WHEN typeof(topic) = 'text' THEN topic END,
                   schema_version,
                   CASE WHEN typeof(entity_key) = 'blob' AND length(entity_key) = 32
                        THEN entity_key END,
                   CASE WHEN typeof(operation) = 'text' THEN operation END,
                   CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND 65536
                        THEN payload END
            FROM change_log
            WHERE commit_seq = ?1 AND ordinal = 0
            "#,
            [to_i64(base_commit, "catalog partial base commit")?],
            |row| {
                Ok(StoredCatalogPartialBaseChange {
                    topic: row.get(0)?,
                    schema_version: row.get(1)?,
                    entity_key: row.get(2)?,
                    operation: row.get(3)?,
                    payload: row.get(4)?,
                })
            },
        )
        .map_err(|error| sqlite_error("load catalog partial base change", error))?;
    let payload = stored.payload.ok_or_else(|| {
        corrupt_catalog_state("catalog partial base invalidation exceeds its byte bound")
    })?;
    let value: serde_json::Value = serde_json::from_slice(&payload).map_err(|_| {
        corrupt_catalog_state("catalog partial base invalidation is not canonical JSON")
    })?;
    let object = value.as_object().ok_or_else(|| {
        corrupt_catalog_state("catalog partial base invalidation is not an object")
    })?;
    let expected_plan_id = serde_json::to_value(plan.coverage_plan_id).map_err(|_| {
        corrupt_catalog_state("catalog partial base plan identity cannot be encoded")
    })?;
    let common_matches = change_count == 1
        && stored.topic.as_deref() == Some(READINESS_CHANGE_TOPIC)
        && stored.entity_key.as_deref() == Some(plan.coverage_plan_id.storage_bytes().as_slice())
        && stored.operation.as_deref() == Some("upsert")
        && object.get("readiness_contract_version")
            == Some(&serde_json::Value::from(CATALOG_READINESS_CONTRACT_VERSION))
        && object.get("scope") == Some(&serde_json::Value::from(LIBRARY_SCOPE))
        && object.get("coverage_plan_id") == Some(&expected_plan_id)
        && object.get("desired_contract_version")
            == Some(&serde_json::Value::from(
                CATALOG_QUERY_PACK_CONTRACT_VERSION,
            ))
        && object.get("epoch") == Some(&serde_json::Value::from(epoch))
        && object.get("attempt") == Some(&serde_json::Value::from(attempt))
        && object.get("state") == Some(&serde_json::Value::from(BUILDING_STATE))
        && object.get("commit_seq") == Some(&serde_json::Value::from(base_commit));
    let generic_keys = [
        "attempt",
        "commit_seq",
        "coverage_plan_id",
        "desired_contract_version",
        "epoch",
        "readiness_contract_version",
        "scope",
        "state",
    ];
    let generic = stored.schema_version == i64::from(READINESS_CHANGE_SCHEMA_VERSION)
        && object.len() == generic_keys.len()
        && generic_keys.iter().all(|key| object.contains_key(*key));
    let recovery_keys = [
        "attempt",
        "commit_seq",
        "completed_contract_version",
        "coverage_plan_id",
        "desired_contract_version",
        "epoch",
        "last_complete_snapshot",
        "readiness_contract_version",
        "scope",
        "state",
    ];
    let recovery_snapshot = object
        .get("last_complete_snapshot")
        .cloned()
        .map(serde_json::from_value::<CatalogSnapshotId>)
        .transpose()
        .map_err(|_| {
            corrupt_catalog_state("catalog partial recovery base has an invalid snapshot")
        })?;
    let recovery = expected_reason == REFRESH_RECOVERY_STARTED_REASON
        && stored.schema_version == i64::from(SOURCE_UNAVAILABLE_CHANGE_SCHEMA_VERSION)
        && object.len() == recovery_keys.len()
        && recovery_keys.iter().all(|key| object.contains_key(*key))
        && object.get("completed_contract_version")
            == Some(&serde_json::Value::from(
                CATALOG_QUERY_PACK_CONTRACT_VERSION,
            ))
        && recovery_snapshot.is_some_and(|snapshot| {
            snapshot.coverage_plan_id == plan.coverage_plan_id
                && snapshot.readiness_epoch == epoch
                && snapshot.pack_contract_version == CATALOG_QUERY_PACK_CONTRACT_VERSION
                && snapshot.complete_commit < base_commit
        });
    let cold_recovery_keys = [
        "attempt",
        "commit_seq",
        "coverage_plan_id",
        "desired_contract_version",
        "epoch",
        "predecessor_state_commit",
        "previous_state",
        "readiness_contract_version",
        "scope",
        "source_count",
        "state",
        "transition",
    ];
    let cold_predecessor = object
        .get("predecessor_state_commit")
        .and_then(serde_json::Value::as_u64);
    let cold_failure_matches = if let Some(predecessor) = cold_predecessor {
        connection
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM catalog_initial_source_failures
                WHERE failure_commit_seq = ?1
                  AND coverage_plan_id = ?2
                  AND readiness_epoch = ?3
                  AND attempt = ?4
                "#,
                params![
                    to_i64(predecessor, "catalog cold recovery predecessor")?,
                    plan.coverage_plan_id.storage_bytes().as_slice(),
                    to_i64(epoch, "catalog cold recovery epoch")?,
                    to_i64(
                        attempt.checked_sub(1).ok_or_else(|| {
                            corrupt_catalog_state("catalog cold recovery attempt underflow")
                        })?,
                        "catalog cold recovery prior attempt",
                    )?,
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| sqlite_error("validate catalog cold recovery predecessor", error))?
            == 1
    } else {
        false
    };
    let cold_recovery = expected_reason == REFRESH_RECOVERY_STARTED_REASON
        && stored.schema_version == i64::from(INITIAL_SOURCE_CHANGE_SCHEMA_VERSION)
        && object.len() == cold_recovery_keys.len()
        && cold_recovery_keys
            .iter()
            .all(|key| object.contains_key(*key))
        && object.get("previous_state") == Some(&serde_json::Value::from(DEGRADED_STATE))
        && object.get("transition") == Some(&serde_json::Value::from("recovery_started"))
        && object.get("source_count") == Some(&serde_json::Value::from(0))
        && cold_predecessor.is_some_and(|predecessor| predecessor < base_commit)
        && cold_failure_matches;
    if !common_matches
        || !((expected_reason == SCHEDULE_REASON && generic)
            || (expected_reason == REFRESH_RECOVERY_STARTED_REASON
                && (generic || recovery || cold_recovery)))
    {
        return Err(corrupt_catalog_state(
            "catalog partial base invalidation does not bind its exact build lineage",
        ));
    }
    Ok(())
}

fn decode_stored_state(
    connection: &Connection,
    stored: StoredCatalogBuildState,
) -> Result<DurableCatalogBuildState, EngineError> {
    let Some(plan_json) = stored.plan_json else {
        return Err(corrupt_catalog_state(
            "catalog coverage-plan JSON is outside its durable byte bound",
        ));
    };
    let coverage_plan_id = stored.coverage_plan_id.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog plan identity exceeds its fixed durable bound")
    })?;
    let content_digest = stored.content_digest.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog plan content digest exceeds its fixed durable bound")
    })?;
    let state = stored.state.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog durable build state is outside its closed vocabulary")
    })?;
    let plan_commit_reason = stored.plan_commit_reason.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog plan commit reason is outside its closed vocabulary")
    })?;
    let state_commit_reason = stored.state_commit_reason.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog state commit reason is outside its closed vocabulary")
    })?;
    if blake3::hash(&plan_json).as_bytes() != content_digest {
        return Err(corrupt_catalog_state(
            "catalog coverage-plan content digest does not match stored bytes",
        ));
    }
    let plan: CatalogCoveragePlan = serde_json::from_slice(&plan_json).map_err(|error| {
        corrupt_catalog_state(format!("catalog coverage-plan JSON is invalid: {error}"))
    })?;
    plan.validate().map_err(catalog_contract_error)?;
    if plan.scope != CatalogCoverageScope::Library
        || plan.coverage_plan_id.storage_bytes().as_slice() != coverage_plan_id
        || i64::from(plan.coverage_plan_contract_version) != stored.coverage_plan_contract_version
    {
        return Err(corrupt_catalog_state(
            "catalog plan row does not match its validated Library plan",
        ));
    }
    let desired_contract_version = positive_u32(
        stored.desired_contract_version,
        "catalog desired contract version",
    )?;
    let epoch = positive_u64(stored.epoch, "catalog readiness epoch")?;
    let attempt = positive_u64(stored.attempt, "catalog readiness attempt")?;
    let phase = CatalogDurableBuildPhase::parse(state)?;
    let created_commit_seq = positive_u64(stored.created_commit_seq, "catalog plan commit")?;
    let last_commit_seq = positive_u64(stored.last_commit_seq, "catalog state commit")?;
    let refreshing_from_snapshot_commit = stored
        .refreshing_from_snapshot_commit
        .map(|value| positive_u64(value, "catalog refreshing-from snapshot commit"))
        .transpose()?;
    let stored_reason_code = stored.reason_code.as_deref();
    if let Some(reason_code) = stored_reason_code {
        validate_reason_code(reason_code).map_err(|_| {
            corrupt_catalog_state("catalog readiness reason is outside its machine-code bound")
        })?;
    }
    let partial_target =
        (phase == CatalogDurableBuildPhase::Partial).then_some(if stored_reason_code.is_some() {
            CatalogPartialCoverageTarget::LatestBefore {
                epoch,
                attempt,
                commit: last_commit_seq,
            }
        } else {
            CatalogPartialCoverageTarget::Exact {
                epoch,
                attempt,
                commit: last_commit_seq,
            }
        });
    let mut partial_coverage =
        load_and_validate_partial_history(connection, &plan, partial_target)?;
    validate_admin_commit(
        stored.plan_commit_source,
        plan_commit_reason,
        stored.plan_committed_at,
        stored.plan_fact_count,
        REGISTER_REASON,
    )?;
    let expected_state_reason =
        if phase == CatalogDurableBuildPhase::Ready && refreshing_from_snapshot_commit.is_none() {
            match state_commit_reason {
                INITIAL_PUBLICATION_REASON => INITIAL_PUBLICATION_REASON,
                REFRESH_PUBLICATION_REASON => REFRESH_PUBLICATION_REASON,
                _ => {
                    return Err(corrupt_catalog_state(
                        "plain Ready catalog state has an unsupported publication owner",
                    ));
                }
            }
        } else if stored_reason_code.is_some()
            && matches!(
                phase,
                CatalogDurableBuildPhase::Ready
                    | CatalogDurableBuildPhase::Building
                    | CatalogDurableBuildPhase::Partial
            )
        {
            if stored.last_complete_snapshot_commit.is_none() {
                INITIAL_SOURCE_RETRYING_REASON
            } else {
                REFRESH_SOURCE_RETRYING_REASON
            }
        } else if phase == CatalogDurableBuildPhase::Degraded {
            if stored.last_complete_snapshot_commit.is_none() {
                INITIAL_SOURCE_UNAVAILABLE_REASON
            } else {
                REFRESH_SOURCE_UNAVAILABLE_REASON
            }
        } else if phase == CatalogDurableBuildPhase::Error {
            if stored.last_complete_snapshot_commit.is_some() {
                REFRESH_INTEGRITY_FAILURE_REASON
            } else {
                INITIAL_INTEGRITY_FAILURE_REASON
            }
        } else if phase == CatalogDurableBuildPhase::Building
            && state_commit_reason == REFRESH_RECOVERY_STARTED_REASON
        {
            REFRESH_RECOVERY_STARTED_REASON
        } else if phase == CatalogDurableBuildPhase::Building
            && state_commit_reason == SOURCE_GENERATION_INVALIDATED_REASON
        {
            SOURCE_GENERATION_INVALIDATED_REASON
        } else {
            phase_reason(phase, refreshing_from_snapshot_commit.is_some())?
        };
    validate_admin_commit(
        stored.state_commit_source,
        state_commit_reason,
        stored.state_committed_at,
        stored.state_fact_count,
        expected_state_reason,
    )?;
    if stored.state_committed_at != Some(stored.updated_at) {
        return Err(corrupt_catalog_state(
            "catalog build-state timestamp does not match its owning commit",
        ));
    }
    match phase {
        CatalogDurableBuildPhase::Pending if last_commit_seq != created_commit_seq => {
            return Err(corrupt_catalog_state(
                "pending catalog build must be owned by its registration commit",
            ));
        }
        CatalogDurableBuildPhase::Building if last_commit_seq <= created_commit_seq => {
            return Err(corrupt_catalog_state(
                "building catalog state must follow its registration commit",
            ));
        }
        CatalogDurableBuildPhase::Partial if last_commit_seq <= created_commit_seq => {
            return Err(corrupt_catalog_state(
                "partial catalog state must follow its registration commit",
            ));
        }
        CatalogDurableBuildPhase::Ready if last_commit_seq <= created_commit_seq => {
            return Err(corrupt_catalog_state(
                "ready catalog publication must follow its registration commit",
            ));
        }
        CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Error
            if last_commit_seq <= created_commit_seq =>
        {
            return Err(corrupt_catalog_state(
                "catalog terminal readiness must follow its registration commit",
            ));
        }
        _ => {}
    }

    let completed_contract_version = stored
        .completed_contract_version
        .map(|value| positive_u32(value, "catalog completed contract version"))
        .transpose()?;
    let complete_through_commit = stored
        .complete_through_commit
        .map(|value| positive_u64(value, "catalog complete-through commit"))
        .transpose()?;
    let last_complete_snapshot_commit = stored
        .last_complete_snapshot_commit
        .map(|value| positive_u64(value, "catalog last-complete snapshot commit"))
        .transpose()?;
    match phase {
        CatalogDurableBuildPhase::Pending
            if completed_contract_version.is_some()
                || complete_through_commit.is_some()
                || last_complete_snapshot_commit.is_some()
                || refreshing_from_snapshot_commit.is_some() =>
        {
            return Err(corrupt_catalog_state(
                "pending catalog state cannot claim completed snapshot fields",
            ));
        }
        CatalogDurableBuildPhase::Building
            if !matches!(
                (
                    completed_contract_version,
                    complete_through_commit,
                    last_complete_snapshot_commit,
                    refreshing_from_snapshot_commit,
                ),
                (None, None, None, None)
            ) && !(completed_contract_version == Some(desired_contract_version)
                && complete_through_commit.is_none()
                && last_complete_snapshot_commit.is_some()
                && refreshing_from_snapshot_commit.is_none()
                && last_complete_snapshot_commit
                    .is_some_and(|commit| last_commit_seq > commit)) =>
        {
            return Err(corrupt_catalog_state(
                "building catalog state is neither an initial build nor an exact retained-snapshot recovery",
            ));
        }
        CatalogDurableBuildPhase::Partial
            if !matches!(
                (
                    completed_contract_version,
                    complete_through_commit,
                    last_complete_snapshot_commit,
                    refreshing_from_snapshot_commit,
                ),
                (None, None, None, None)
            ) && !(completed_contract_version == Some(desired_contract_version)
                && complete_through_commit.is_none()
                && last_complete_snapshot_commit.is_some()
                && refreshing_from_snapshot_commit.is_none()
                && last_complete_snapshot_commit
                    .is_some_and(|commit| last_commit_seq > commit)) =>
        {
            return Err(corrupt_catalog_state(
                "partial catalog state has an invalid retained-snapshot shape",
            ));
        }
        CatalogDurableBuildPhase::Ready => match refreshing_from_snapshot_commit {
            None if completed_contract_version != Some(desired_contract_version)
                || complete_through_commit != Some(last_commit_seq)
                || last_complete_snapshot_commit != Some(last_commit_seq) =>
            {
                return Err(corrupt_catalog_state(
                    "plain ready catalog state does not identify its exact completed snapshot",
                ));
            }
            Some(refreshing_commit)
                if completed_contract_version != Some(desired_contract_version)
                    || complete_through_commit != Some(refreshing_commit)
                    || last_complete_snapshot_commit != Some(refreshing_commit)
                    || last_commit_seq <= refreshing_commit =>
            {
                return Err(corrupt_catalog_state(
                    "refreshing ready catalog state does not retain its exact completed snapshot",
                ));
            }
            _ => {}
        },
        CatalogDurableBuildPhase::Degraded => {
            let cold = matches!(
                (
                    completed_contract_version,
                    complete_through_commit,
                    last_complete_snapshot_commit,
                    refreshing_from_snapshot_commit,
                ),
                (None, None, None, None)
            );
            let retained = completed_contract_version == Some(desired_contract_version)
                && complete_through_commit
                    .is_none_or(|commit| Some(commit) == last_complete_snapshot_commit)
                && refreshing_from_snapshot_commit.is_none()
                && last_complete_snapshot_commit.is_some_and(|commit| last_commit_seq > commit);
            if !cold && !retained {
                return Err(corrupt_catalog_state(
                    "degraded catalog readiness is neither cold nor bound to one exact prior snapshot",
                ));
            }
        }
        CatalogDurableBuildPhase::Error
            if !(completed_contract_version == Some(desired_contract_version)
                && last_complete_snapshot_commit.is_some()
                && (complete_through_commit.is_none()
                    || complete_through_commit == last_complete_snapshot_commit)
                && refreshing_from_snapshot_commit.is_none()
                && last_complete_snapshot_commit
                    .is_some_and(|commit| last_commit_seq > commit))
                && !matches!(
                    (
                        completed_contract_version,
                        complete_through_commit,
                        last_complete_snapshot_commit,
                        refreshing_from_snapshot_commit,
                    ),
                    (None, None, None, None)
                ) =>
        {
            return Err(corrupt_catalog_state(
                "catalog Error is neither independently-safe nor one exact discarded initial build",
            ));
        }
        _ => {}
    }
    let reason_shape_is_valid = match phase {
        CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Error => {
            stored_reason_code.is_some()
        }
        CatalogDurableBuildPhase::Building | CatalogDurableBuildPhase::Partial => true,
        CatalogDurableBuildPhase::Ready => {
            refreshing_from_snapshot_commit.is_some() || stored_reason_code.is_none()
        }
        CatalogDurableBuildPhase::Pending => stored_reason_code.is_none(),
    };
    if !reason_shape_is_valid {
        return Err(corrupt_catalog_state(
            "catalog readiness reason does not match its durable phase",
        ));
    }

    let mut machine = CatalogReadinessMachine::register(plan.clone(), desired_contract_version)
        .map_err(catalog_contract_error)?;
    if phase != CatalogDurableBuildPhase::Pending {
        machine.schedule_build().map_err(catalog_contract_error)?;
    }
    let (
        ready_publication_identity,
        ready_publication_coverage,
        ready_publication_attempt,
        recovery_origin,
        epoch_replacement,
        retired_snapshot_count,
    ) = if let Some(snapshot_commit) = last_complete_snapshot_commit {
        let snapshot_id = load_snapshot_id_at_commit(connection, &plan, snapshot_commit)?;
        if completed_contract_version != Some(snapshot_id.pack_contract_version) {
            return Err(corrupt_catalog_state(
                "catalog completed contract differs from its retained snapshot",
            ));
        }
        let publication_attempt: i64 = connection
            .query_row(
                "SELECT attempt FROM catalog_snapshots WHERE snapshot_commit_seq = ?1",
                [to_i64(snapshot_commit, "catalog retained snapshot commit")?],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_error("load catalog retained snapshot attempt", error))?;
        let publication_attempt =
            positive_u64(publication_attempt, "catalog retained snapshot attempt")?;
        let publication = super::catalog_publication::load_ready_publication(
            connection,
            &plan,
            snapshot_id,
            publication_attempt,
        )?;
        if !publication.identity.is_refresh() {
            validate_initial_snapshot_epoch_replacement(
                connection,
                &plan,
                snapshot_id,
                publication_attempt,
                publication.identity.build_commit_seq(),
            )?;
        }
        let retained_snapshot_scan_limit =
            super::catalog_publication::MAX_RETAINED_REFRESH_LINEAGE_DEPTH
                .checked_add(2)
                .ok_or_else(|| {
                    corrupt_catalog_state("catalog retained snapshot scan limit overflow")
                })?;
        let retained_snapshot_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM (SELECT 1 FROM catalog_snapshots LIMIT ?1)",
                [to_i64(
                    retained_snapshot_scan_limit as u64,
                    "catalog retained snapshot scan limit",
                )?],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_error("count retained catalog snapshots", error))?;
        if usize::try_from(retained_snapshot_count).ok()
            != Some(publication.identity.retained_snapshot_count())
        {
            return Err(corrupt_catalog_state(
                "retained catalog snapshots do not form the exact bounded current ancestor chain",
            ));
        }
        if phase == CatalogDurableBuildPhase::Ready
            && refreshing_from_snapshot_commit.is_none()
            && publication.identity.is_refresh()
                != (state_commit_reason == REFRESH_PUBLICATION_REASON)
        {
            return Err(corrupt_catalog_state(
                "Ready state commit owner disagrees with the current snapshot lineage",
            ));
        }
        let retired_snapshot_count = super::catalog_retention::load_retired_prefix(
            connection,
            publication.identity.retained_chain(),
        )?;
        let publication_coverage = publication.source_coverage;
        let source_failure = load_latest_source_failure_evidence(connection)?;
        let initial_source_failure = load_latest_initial_source_failure_evidence(connection)?;
        let integrity_failure = load_latest_integrity_failure_evidence(connection)?;
        if phase != CatalogDurableBuildPhase::Degraded {
            if let Some(failure) = source_failure.as_ref() {
                let failure_attempt =
                    positive_u64(failure.attempt, "catalog source-failure attempt")?;
                let failure_epoch =
                    positive_u64(failure.readiness_epoch, "catalog source-failure epoch")?;
                let failure_commit =
                    positive_u64(failure.failure_commit_seq, "catalog source-failure commit")?;
                if failure_epoch > epoch
                    || (failure_epoch == epoch && failure_attempt >= attempt)
                    || failure_commit >= last_commit_seq
                {
                    return Err(corrupt_catalog_state(
                        "historical catalog source failure is not behind current readiness",
                    ));
                }
            }
        }
        if phase != CatalogDurableBuildPhase::Error {
            if let Some(failure) = integrity_failure.as_ref() {
                let failure_attempt =
                    positive_u64(failure.attempt, "catalog integrity-failure attempt")?;
                let failure_epoch =
                    positive_u64(failure.readiness_epoch, "catalog integrity-failure epoch")?;
                let failure_commit = positive_u64(
                    failure.failure_commit_seq,
                    "catalog integrity-failure commit",
                )?;
                if failure_epoch > epoch
                    || (failure_epoch == epoch && failure_attempt >= attempt)
                    || failure_commit >= last_commit_seq
                {
                    return Err(corrupt_catalog_state(
                        "historical catalog integrity failure is not behind current readiness",
                    ));
                }
            }
        }
        if let Some(failure) = initial_source_failure.as_ref() {
            let failure_attempt =
                positive_u64(failure.attempt, "catalog initial source-failure attempt")?;
            let failure_epoch = positive_u64(
                failure.readiness_epoch,
                "catalog initial source-failure epoch",
            )?;
            let failure_commit = positive_u64(
                failure.failure_commit_seq,
                "catalog initial source-failure commit",
            )?;
            if failure_epoch > epoch
                || (failure_epoch == epoch && failure_attempt >= attempt)
                || failure_commit >= last_commit_seq
            {
                return Err(corrupt_catalog_state(
                    "historical initial catalog source failure is not behind current readiness",
                ));
            }
        }
        if phase == CatalogDurableBuildPhase::Ready
            && !publication.identity.is_refresh()
            && publication_attempt > 1
        {
            validate_cold_recovery_origin(
                connection,
                CatalogColdRecoveryRestartContext {
                    initial_source_failure: initial_source_failure.as_ref(),
                    integrity_failure: integrity_failure.as_ref(),
                    plan: &plan,
                    epoch,
                    attempt: publication_attempt,
                    state_commit_seq: publication.identity.build_commit_seq(),
                    updated_at: stored.updated_at,
                },
            )?;
        }
        let terminal_epoch_replacement = matches!(
            phase,
            CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Error
        ) && epoch > snapshot_id.readiness_epoch;
        if terminal_epoch_replacement {
            let invalidation = load_and_validate_epoch_invalidation(
                connection,
                &plan,
                desired_contract_version,
                epoch,
                Some(snapshot_id),
            )?
            .ok_or_else(|| {
                corrupt_catalog_state(
                    "catalog terminal replacement epoch is missing source-generation invalidation evidence",
                )
            })?;
            if invalidation >= last_commit_seq {
                return Err(corrupt_catalog_state(
                    "catalog terminal replacement does not descend from its exact invalidation",
                ));
            }
        }
        let (snapshot_state, source_coverage, readiness_reason, recovery_origin, epoch_replacement) =
            if phase == CatalogDurableBuildPhase::Error {
                let failure = integrity_failure.as_ref().ok_or_else(|| {
                    corrupt_catalog_state(
                        "catalog Error is missing its exact integrity-failure evidence",
                    )
                })?;
                let evidence_reason = validate_integrity_failure_for_restart(
                    failure,
                    CatalogIntegrityRestartContext {
                        plan: &plan,
                        state_epoch: epoch,
                        state_attempt: attempt,
                        state_commit_seq: last_commit_seq,
                        updated_at: stored.updated_at,
                        retained_snapshot: snapshot_id,
                        publication_identity: &publication.identity,
                        publication_attempt,
                        recovering: false,
                    },
                )?;
                if stored_reason_code != Some(evidence_reason.as_str()) {
                    return Err(corrupt_catalog_state(
                        "catalog Error reason differs from integrity-failure evidence",
                    ));
                }
                (
                    CatalogReadinessPhase::Error,
                    publication_coverage.clone(),
                    Some(CatalogReadinessReason::IntegrityFailure {
                        code: evidence_reason,
                        snapshot_disposition:
                            CatalogIntegritySnapshotDisposition::IndependentlySafe,
                    }),
                    None,
                    terminal_epoch_replacement,
                )
            } else if phase == CatalogDurableBuildPhase::Degraded {
                let failure = source_failure.as_ref().ok_or_else(|| {
                    corrupt_catalog_state(
                        "catalog degraded state is missing source-failure evidence",
                    )
                })?;
                let (_, evidence_reason) = validate_source_failure_for_restart(
                    failure,
                    CatalogSourceFailureRestartContext {
                        plan: &plan,
                        state_epoch: epoch,
                        state_attempt: attempt,
                        state_commit_seq: last_commit_seq,
                        updated_at: stored.updated_at,
                        retained_snapshot: snapshot_id,
                        publication_identity: &publication.identity,
                        publication_attempt,
                        recovering: false,
                    },
                )?;
                if stored_reason_code != Some(evidence_reason.as_str()) {
                    return Err(corrupt_catalog_state(
                        "catalog degraded reason differs from source-failure evidence",
                    ));
                }
                (
                    CatalogReadinessPhase::Degraded,
                    unavailable_source_coverage(&publication_coverage)?,
                    Some(CatalogReadinessReason::TerminalSourceUnavailable {
                        code: evidence_reason,
                    }),
                    None,
                    terminal_epoch_replacement,
                )
            } else if matches!(
                phase,
                CatalogDurableBuildPhase::Building | CatalogDurableBuildPhase::Partial
            ) {
                if epoch > snapshot_id.readiness_epoch {
                    let invalidation = load_and_validate_epoch_invalidation(
                    connection,
                    &plan,
                    desired_contract_version,
                    epoch,
                    Some(snapshot_id),
                )?
                .ok_or_else(|| {
                    corrupt_catalog_state(
                        "catalog replacement epoch is missing source-generation invalidation evidence",
                    )
                })?;
                    if (attempt == 1
                        && ((phase == CatalogDurableBuildPhase::Building
                            && invalidation != last_commit_seq)
                            || (phase == CatalogDurableBuildPhase::Partial
                                && invalidation >= last_commit_seq)))
                        || (attempt > 1 && invalidation >= last_commit_seq)
                    {
                        return Err(corrupt_catalog_state(
                            "catalog replacement epoch does not descend from its exact invalidation",
                        ));
                    }
                    let recovery_origin = if attempt == 1 {
                        None
                    } else {
                        let prior_attempt = attempt.checked_sub(1).ok_or_else(|| {
                            corrupt_catalog_state("catalog recovery attempt cannot be decremented")
                        })?;
                        let source_matches = source_failure
                            .as_ref()
                            .map(|failure| {
                                positive_u64(failure.attempt, "catalog source-failure attempt")
                                    .map(|value| value == prior_attempt)
                            })
                            .transpose()?
                            .unwrap_or(false);
                        let integrity_matches = integrity_failure
                            .as_ref()
                            .map(|failure| {
                                positive_u64(failure.attempt, "catalog integrity-failure attempt")
                                    .map(|value| value == prior_attempt)
                            })
                            .transpose()?
                            .unwrap_or(false);
                        Some(
                            match (
                                source_matches,
                                integrity_matches,
                                source_failure.as_ref(),
                                integrity_failure.as_ref(),
                            ) {
                                (true, false, Some(failure), _) => {
                                    validate_source_failure_for_restart(
                                        failure,
                                        CatalogSourceFailureRestartContext {
                                            plan: &plan,
                                            state_epoch: epoch,
                                            state_attempt: attempt,
                                            state_commit_seq: last_commit_seq,
                                            updated_at: stored.updated_at,
                                            retained_snapshot: snapshot_id,
                                            publication_identity: &publication.identity,
                                            publication_attempt,
                                            recovering: true,
                                        },
                                    )?;
                                    CatalogDurableBuildPhase::Degraded
                                }
                                (false, true, _, Some(failure)) => {
                                    validate_integrity_failure_for_restart(
                                        failure,
                                        CatalogIntegrityRestartContext {
                                            plan: &plan,
                                            state_epoch: epoch,
                                            state_attempt: attempt,
                                            state_commit_seq: last_commit_seq,
                                            updated_at: stored.updated_at,
                                            retained_snapshot: snapshot_id,
                                            publication_identity: &publication.identity,
                                            publication_attempt,
                                            recovering: true,
                                        },
                                    )?;
                                    CatalogDurableBuildPhase::Error
                                }
                                _ => {
                                    return Err(corrupt_catalog_state(
                                    "catalog replacement recovery does not identify exactly one prior terminal failure",
                                ));
                                }
                            },
                        )
                    };
                    let replacement_reason =
                        stored_reason_code.map(|code| CatalogReadinessReason::SourceRetrying {
                            code: code.to_string(),
                        });
                    let replacement_coverage = if phase == CatalogDurableBuildPhase::Partial {
                        let milestone = partial_coverage.take().ok_or_else(|| {
                            corrupt_catalog_state(
                            "catalog replacement Partial state is missing its coverage milestone",
                        )
                        })?;
                        if replacement_reason.is_some() {
                            retrying_source_coverage(&milestone)?
                        } else {
                            milestone
                        }
                    } else if attempt == 1 {
                        Vec::new()
                    } else {
                        unavailable_source_coverage(&publication_coverage)?
                    };
                    (
                        if phase == CatalogDurableBuildPhase::Partial {
                            CatalogReadinessPhase::Partial
                        } else {
                            CatalogReadinessPhase::Building
                        },
                        replacement_coverage,
                        replacement_reason,
                        recovery_origin,
                        true,
                    )
                } else {
                    let prior_attempt = attempt.checked_sub(1).ok_or_else(|| {
                        corrupt_catalog_state("catalog recovery attempt cannot be decremented")
                    })?;
                    let source_matches = source_failure
                        .as_ref()
                        .map(|failure| {
                            positive_u64(failure.attempt, "catalog source-failure attempt")
                                .map(|value| value == prior_attempt)
                        })
                        .transpose()?
                        .unwrap_or(false);
                    let integrity_matches = integrity_failure
                        .as_ref()
                        .map(|failure| {
                            positive_u64(failure.attempt, "catalog integrity-failure attempt")
                                .map(|value| value == prior_attempt)
                        })
                        .transpose()?
                        .unwrap_or(false);
                    let recovery_origin = match (
                        source_matches,
                        integrity_matches,
                        source_failure.as_ref(),
                        integrity_failure.as_ref(),
                    ) {
                        (true, false, Some(failure), _) => {
                            validate_source_failure_for_restart(
                                failure,
                                CatalogSourceFailureRestartContext {
                                    plan: &plan,
                                    state_epoch: epoch,
                                    state_attempt: attempt,
                                    state_commit_seq: last_commit_seq,
                                    updated_at: stored.updated_at,
                                    retained_snapshot: snapshot_id,
                                    publication_identity: &publication.identity,
                                    publication_attempt,
                                    recovering: true,
                                },
                            )?;
                            CatalogDurableBuildPhase::Degraded
                        }
                        (false, true, _, Some(failure)) => {
                            validate_integrity_failure_for_restart(
                                failure,
                                CatalogIntegrityRestartContext {
                                    plan: &plan,
                                    state_epoch: epoch,
                                    state_attempt: attempt,
                                    state_commit_seq: last_commit_seq,
                                    updated_at: stored.updated_at,
                                    retained_snapshot: snapshot_id,
                                    publication_identity: &publication.identity,
                                    publication_attempt,
                                    recovering: true,
                                },
                            )?;
                            CatalogDurableBuildPhase::Error
                        }
                        _ => {
                            return Err(corrupt_catalog_state(
                        "catalog recovery does not identify exactly one prior terminal failure",
                    ));
                        }
                    };
                    let recovery_reason =
                        stored_reason_code.map(|code| CatalogReadinessReason::SourceRetrying {
                            code: code.to_string(),
                        });
                    let current_coverage = if phase == CatalogDurableBuildPhase::Partial {
                        let milestone = partial_coverage.take().ok_or_else(|| {
                            corrupt_catalog_state(
                                "catalog Partial recovery is missing its exact coverage milestone",
                            )
                        })?;
                        if recovery_reason.is_some() {
                            retrying_source_coverage(&milestone)?
                        } else {
                            milestone
                        }
                    } else {
                        unavailable_source_coverage(&publication_coverage)?
                    };
                    (
                        if phase == CatalogDurableBuildPhase::Partial {
                            CatalogReadinessPhase::Partial
                        } else {
                            CatalogReadinessPhase::Building
                        },
                        current_coverage,
                        recovery_reason,
                        Some(recovery_origin),
                        false,
                    )
                }
            } else {
                if publication_attempt != attempt {
                    return Err(corrupt_catalog_state(
                        "Ready catalog attempt differs from its publication attempt",
                    ));
                }
                let ready_reason =
                    stored_reason_code.map(|code| CatalogReadinessReason::SourceRetrying {
                        code: code.to_string(),
                    });
                let current_coverage = if ready_reason.is_some() {
                    retrying_source_coverage(&publication_coverage)?
                } else {
                    publication_coverage.clone()
                };
                (
                    CatalogReadinessPhase::Ready,
                    current_coverage,
                    ready_reason,
                    None,
                    false,
                )
            };
        let reconstructed = CatalogReadinessSnapshot {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: CatalogCoverageScope::Library,
            coverage_plan_id: plan.coverage_plan_id,
            desired_contract_version,
            completed_contract_version,
            epoch,
            attempt,
            state: snapshot_state,
            complete_through_commit,
            last_complete_snapshot: Some(snapshot_id),
            refreshing_from_snapshot: refreshing_from_snapshot_commit.map(|_| snapshot_id),
            source_coverage,
            reason: readiness_reason,
        };
        machine = CatalogReadinessMachine::resume(plan.clone(), reconstructed)
            .map_err(catalog_contract_error)?;
        (
            Some(Arc::new(publication.identity)),
            Some(publication_coverage),
            Some(publication_attempt),
            recovery_origin,
            epoch_replacement,
            retired_snapshot_count,
        )
    } else {
        if load_latest_source_failure_evidence(connection)?.is_some() {
            return Err(corrupt_catalog_state(
                "catalog state without a retained snapshot contains retained-refresh source-failure evidence",
            ));
        }
        let initial_source_failure = load_latest_initial_source_failure_evidence(connection)?;
        let integrity_failure = load_latest_integrity_failure_evidence(connection)?;
        let no_snapshot_epoch_replacement = epoch > 1;
        let epoch_invalidation = if no_snapshot_epoch_replacement {
            Some(
                load_and_validate_epoch_invalidation(
                    connection,
                    &plan,
                    desired_contract_version,
                    epoch,
                    None,
                )?
                .ok_or_else(|| {
                    corrupt_catalog_state(
                        "no-snapshot replacement epoch is missing source-generation invalidation evidence",
                    )
                })?,
            )
        } else {
            None
        };
        if let Some(invalidation) = epoch_invalidation {
            let descends_from_invalidation = match (phase, attempt) {
                (CatalogDurableBuildPhase::Building, 1) => invalidation == last_commit_seq,
                (
                    CatalogDurableBuildPhase::Building
                    | CatalogDurableBuildPhase::Partial
                    | CatalogDurableBuildPhase::Degraded
                    | CatalogDurableBuildPhase::Error,
                    _,
                ) => invalidation < last_commit_seq,
                _ => false,
            };
            if !descends_from_invalidation {
                return Err(corrupt_catalog_state(
                    "no-snapshot replacement state does not descend from its exact invalidation",
                ));
            }
        }
        let recovery_origin = match phase {
            CatalogDurableBuildPhase::Pending => None,
            CatalogDurableBuildPhase::Building | CatalogDurableBuildPhase::Partial => {
                let recovery_origin = (attempt > 1)
                    .then(|| {
                        validate_cold_recovery_origin(
                            connection,
                            CatalogColdRecoveryRestartContext {
                                initial_source_failure: initial_source_failure.as_ref(),
                                integrity_failure: integrity_failure.as_ref(),
                                plan: &plan,
                                epoch,
                                attempt,
                                state_commit_seq: last_commit_seq,
                                updated_at: stored.updated_at,
                            },
                        )
                    })
                    .transpose()?;
                let coverage = if phase == CatalogDurableBuildPhase::Partial {
                    partial_coverage.take().ok_or_else(|| {
                        corrupt_catalog_state(
                            "catalog Partial readiness is missing its exact coverage milestone",
                        )
                    })?
                } else {
                    Vec::new()
                };
                let reason =
                    stored_reason_code.map(|code| CatalogReadinessReason::SourceRetrying {
                        code: code.to_string(),
                    });
                let current_coverage = if reason.is_some() {
                    retrying_source_coverage(&coverage)?
                } else {
                    coverage
                };
                if let Some(code) = stored_reason_code {
                    validate_initial_source_change(
                        connection,
                        last_commit_seq,
                        &plan,
                        desired_contract_version,
                        epoch,
                        attempt,
                        phase,
                        phase.readiness_phase(),
                        "source_retrying",
                        current_coverage.len(),
                        Some(code),
                    )?;
                } else if phase == CatalogDurableBuildPhase::Building
                    && recovery_origin == Some(CatalogDurableBuildPhase::Degraded)
                {
                    validate_initial_source_change(
                        connection,
                        last_commit_seq,
                        &plan,
                        desired_contract_version,
                        epoch,
                        attempt,
                        CatalogDurableBuildPhase::Degraded,
                        CatalogReadinessPhase::Building,
                        "recovery_started",
                        0,
                        None,
                    )?;
                }
                machine = CatalogReadinessMachine::resume(
                    plan.clone(),
                    CatalogReadinessSnapshot {
                        readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
                        scope: CatalogCoverageScope::Library,
                        coverage_plan_id: plan.coverage_plan_id,
                        desired_contract_version,
                        completed_contract_version: None,
                        epoch,
                        attempt,
                        state: phase.readiness_phase(),
                        complete_through_commit: None,
                        last_complete_snapshot: None,
                        refreshing_from_snapshot: None,
                        source_coverage: current_coverage,
                        reason,
                    },
                )
                .map_err(catalog_contract_error)?;
                recovery_origin
            }
            CatalogDurableBuildPhase::Degraded => {
                let failure = initial_source_failure.as_ref().ok_or_else(|| {
                    corrupt_catalog_state(
                        "cold degraded catalog is missing initial source-failure evidence",
                    )
                })?;
                let (previous_state, evidence_reason) =
                    validate_initial_source_failure_for_restart(
                        failure,
                        CatalogInitialSourceFailureRestartContext {
                            plan: &plan,
                            state_epoch: epoch,
                            state_attempt: attempt,
                            state_commit_seq: last_commit_seq,
                            updated_at: stored.updated_at,
                            recovering: false,
                        },
                    )?;
                if stored_reason_code != Some(evidence_reason.as_str()) {
                    return Err(corrupt_catalog_state(
                        "cold degraded reason differs from source-failure evidence",
                    ));
                }
                let prior_coverage = if previous_state == CatalogDurableBuildPhase::Partial {
                    load_and_validate_partial_history(
                        connection,
                        &plan,
                        Some(CatalogPartialCoverageTarget::LatestBefore {
                            epoch,
                            attempt,
                            commit: last_commit_seq,
                        }),
                    )?
                    .ok_or_else(|| {
                        corrupt_catalog_state(
                            "cold degraded catalog is missing its prior Partial milestone",
                        )
                    })?
                } else {
                    Vec::new()
                };
                let source_coverage = unavailable_initial_source_coverage(
                    &plan,
                    desired_contract_version,
                    &prior_coverage,
                )?;
                let predecessor = validate_initial_source_change(
                    connection,
                    last_commit_seq,
                    &plan,
                    desired_contract_version,
                    epoch,
                    attempt,
                    previous_state,
                    CatalogReadinessPhase::Degraded,
                    "source_unavailable",
                    source_coverage.len(),
                    Some(&evidence_reason),
                )?;
                if predecessor
                    != positive_u64(
                        failure.failed_build_commit_seq,
                        "catalog failed initial-source build commit",
                    )?
                {
                    return Err(corrupt_catalog_state(
                        "cold degraded source change differs from its failure predecessor",
                    ));
                }
                machine = CatalogReadinessMachine::resume(
                    plan.clone(),
                    CatalogReadinessSnapshot {
                        readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
                        scope: CatalogCoverageScope::Library,
                        coverage_plan_id: plan.coverage_plan_id,
                        desired_contract_version,
                        completed_contract_version: None,
                        epoch,
                        attempt,
                        state: CatalogReadinessPhase::Degraded,
                        complete_through_commit: None,
                        last_complete_snapshot: None,
                        refreshing_from_snapshot: None,
                        source_coverage,
                        reason: Some(CatalogReadinessReason::TerminalSourceUnavailable {
                            code: evidence_reason,
                        }),
                    },
                )
                .map_err(catalog_contract_error)?;
                None
            }
            CatalogDurableBuildPhase::Error => {
                let failure = integrity_failure.as_ref().ok_or_else(|| {
                    corrupt_catalog_state(
                        "discarded catalog Error is missing integrity-failure evidence",
                    )
                })?;
                let evidence_reason = validate_discarded_integrity_failure_for_restart(
                    failure,
                    CatalogDiscardedIntegrityRestartContext {
                        plan: &plan,
                        state_epoch: epoch,
                        state_attempt: attempt,
                        state_commit_seq: last_commit_seq,
                        updated_at: stored.updated_at,
                        recovering: false,
                    },
                )?;
                if stored_reason_code != Some(evidence_reason.as_str()) {
                    return Err(corrupt_catalog_state(
                        "discarded catalog Error reason differs from integrity-failure evidence",
                    ));
                }
                machine = CatalogReadinessMachine::resume(
                    plan.clone(),
                    CatalogReadinessSnapshot {
                        readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
                        scope: CatalogCoverageScope::Library,
                        coverage_plan_id: plan.coverage_plan_id,
                        desired_contract_version,
                        completed_contract_version: None,
                        epoch,
                        attempt,
                        state: CatalogReadinessPhase::Error,
                        complete_through_commit: None,
                        last_complete_snapshot: None,
                        refreshing_from_snapshot: None,
                        source_coverage: Vec::new(),
                        reason: Some(CatalogReadinessReason::IntegrityFailure {
                            code: evidence_reason,
                            snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
                        }),
                    },
                )
                .map_err(catalog_contract_error)?;
                None
            }
            CatalogDurableBuildPhase::Ready => {
                return Err(corrupt_catalog_state(
                    "Ready catalog state cannot omit its retained snapshot",
                ));
            }
        };
        (
            None,
            None,
            None,
            recovery_origin,
            no_snapshot_epoch_replacement,
            super::catalog_retention::load_retired_prefix(connection, &[])?,
        )
    };
    if machine.snapshot().epoch != epoch || machine.snapshot().attempt != attempt {
        return Err(corrupt_catalog_state(
            "catalog build epoch or attempt cannot be reconstructed from the persisted lineage",
        ));
    }
    Ok(DurableCatalogBuildState {
        plan,
        readiness: machine.snapshot().clone(),
        last_commit_seq,
        ready_publication_identity,
        ready_publication_coverage,
        ready_publication_attempt,
        recovery_origin,
        epoch_replacement,
        retired_snapshot_count,
    })
}

fn validate_command(command: &CatalogBuildStateCommand) -> Result<(), EngineError> {
    let (started_at, committed_at) = match command {
        CatalogBuildStateCommand::Register {
            plan,
            desired_contract_version,
            started_at,
            committed_at,
        } => {
            plan.validate().map_err(catalog_contract_error)?;
            if plan.scope != CatalogCoverageScope::Library {
                return Err(EngineError::InvalidCommit(
                    "this catalog persistence slice accepts only the Library scope".to_string(),
                ));
            }
            if *desired_contract_version == 0 {
                return Err(EngineError::InvalidCommit(
                    "catalog desired contract version must be greater than zero".to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::MarkInitialBuildSourceRetrying {
            expected,
            reason_code,
            started_at,
            committed_at,
        }
        | CatalogBuildStateCommand::DegradeInitialBuildSource {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            validate_reason_code(reason_code).map_err(catalog_contract_error)?;
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.build_state_commit_seq == 0
                || !matches!(
                    expected.state,
                    CatalogDurableBuildPhase::Building | CatalogDurableBuildPhase::Partial
                )
            {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source transition is outside one exact active-build lineage"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::InvalidateSourceGeneration {
            expected,
            started_at,
            committed_at,
        } => {
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.state_commit_seq == 0
                || expected.state == CatalogDurableBuildPhase::Error
                || expected
                    .last_complete_snapshot
                    .is_some_and(|snapshot| snapshot.complete_commit > expected.state_commit_seq)
            {
                return Err(EngineError::InvalidCommit(
                    "catalog source-generation invalidation is outside one exact non-error Library lineage"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::RecordPartial {
            expected,
            source_coverage,
            started_at,
            committed_at,
        } => {
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.state_commit_seq == 0
                || !matches!(
                    expected.state,
                    CatalogDurableBuildPhase::Building | CatalogDurableBuildPhase::Partial
                )
            {
                return Err(EngineError::InvalidCommit(
                    "catalog partial expectation is outside one active Library build lineage"
                        .to_string(),
                ));
            }
            if source_coverage.is_empty() || source_coverage.len() > MAX_PARTIAL_SOURCES {
                return Err(EngineError::InvalidCommit(
                    "catalog partial coverage source count is empty or unbounded".to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::MarkActiveRefreshRetrying {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            validate_reason_code(reason_code).map_err(catalog_contract_error)?;
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.refresh_started_commit_seq == 0
                || expected.predecessor_snapshot.pack_contract_version
                    != expected.desired_contract_version
                || expected.predecessor_snapshot.coverage_plan_id != expected.coverage_plan_id
                || !active_refresh_epoch_lineage_matches(expected)
                || expected.predecessor_snapshot.complete_commit
                    >= expected.refresh_started_commit_seq
            {
                return Err(EngineError::InvalidCommit(
                    "catalog source retry is outside one exact refresh lineage".to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::Schedule {
            expected,
            started_at,
            committed_at,
        } => {
            if expected.scope != CatalogCoverageScope::Library {
                return Err(EngineError::InvalidCommit(
                    "catalog build expectation is bound to a non-Library scope".to_string(),
                ));
            }
            if expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.state_commit_seq == 0
            {
                return Err(EngineError::InvalidCommit(
                    "catalog build expectation versions, epoch, and attempt must be positive"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::BeginRefresh {
            expected,
            started_at,
            committed_at,
        } => {
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.state_commit_seq == 0
                || expected.snapshot_id.pack_contract_version != expected.desired_contract_version
                || expected.snapshot_id.coverage_plan_id != expected.coverage_plan_id
                || expected.snapshot_id.readiness_epoch != expected.epoch
                || expected.snapshot_id.complete_commit != expected.state_commit_seq
            {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh expectation is outside one exact plain Ready lineage"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::FailInitialBuildIntegrity {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            validate_reason_code(reason_code).map_err(catalog_contract_error)?;
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.build_started_commit_seq == 0
                || !matches!(
                    expected.state,
                    CatalogDurableBuildPhase::Building | CatalogDurableBuildPhase::Partial
                )
            {
                return Err(EngineError::InvalidCommit(
                    "catalog initial integrity failure is outside one exact active-build lineage"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::FailActiveRefreshIntegrity {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            validate_reason_code(reason_code).map_err(catalog_contract_error)?;
            let retained = expected
                .publication_identity
                .retained_chain()
                .last()
                .copied()
                .ok_or_else(|| {
                    EngineError::InvalidCommit(
                        "catalog refresh integrity failure is missing its retained publication"
                            .to_string(),
                    )
                })?;
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.refresh_started_commit_seq == 0
                || expected.predecessor_snapshot.pack_contract_version
                    != expected.desired_contract_version
                || expected.predecessor_snapshot.coverage_plan_id != expected.coverage_plan_id
                || !active_refresh_epoch_lineage_matches(expected)
                || expected.predecessor_snapshot.complete_commit
                    >= expected.refresh_started_commit_seq
                || retained.snapshot_id() != expected.predecessor_snapshot
            {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh integrity failure is outside one exact active refresh lineage"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::DegradeActiveRefresh {
            expected,
            reason_code,
            started_at,
            committed_at,
        } => {
            validate_reason_code(reason_code).map_err(catalog_contract_error)?;
            let retained = expected
                .publication_identity
                .retained_chain()
                .last()
                .copied()
                .ok_or_else(|| {
                    EngineError::InvalidCommit(
                        "catalog source failure is missing its retained publication".to_string(),
                    )
                })?;
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.refresh_started_commit_seq == 0
                || expected.predecessor_snapshot.pack_contract_version
                    != expected.desired_contract_version
                || expected.predecessor_snapshot.coverage_plan_id != expected.coverage_plan_id
                || !active_refresh_epoch_lineage_matches(expected)
                || expected.predecessor_snapshot.complete_commit
                    >= expected.refresh_started_commit_seq
                || retained.snapshot_id() != expected.predecessor_snapshot
            {
                return Err(EngineError::InvalidCommit(
                    "catalog source failure is outside one exact active refresh lineage"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
        CatalogBuildStateCommand::RetryTerminalRefresh {
            expected,
            started_at,
            committed_at,
        } => {
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.state_commit_seq == 0
                || !matches!(
                    expected.state,
                    CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Error
                )
            {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery expectation is outside one terminal Library lineage"
                        .to_string(),
                ));
            }
            (*started_at, *committed_at)
        }
    };
    if committed_at < started_at {
        return Err(EngineError::InvalidCommit(
            "catalog build-state commit time must not precede its start".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn insert_administrative_commit(
    transaction: &Transaction<'_>,
    reason: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<u64, EngineError> {
    let commit_seq: i64 = transaction
        .query_row(
            r#"
            INSERT INTO ingest_commits (
                source_instance_id, reason, started_at, committed_at, fact_count
            ) VALUES (NULL, ?1, ?2, ?3, 0)
            RETURNING commit_seq
            "#,
            params![reason, started_at, committed_at],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("insert catalog administrative commit", error))?;
    positive_u64(commit_seq, "catalog administrative commit")
}

fn insert_plan(
    transaction: &Transaction<'_>,
    plan: &CatalogCoveragePlan,
    commit_seq: u64,
) -> Result<(), EngineError> {
    let plan_json = serde_json::to_vec(plan).map_err(|error| {
        EngineError::InvalidCommit(format!("could not encode catalog coverage plan: {error}"))
    })?;
    if plan_json.is_empty() || plan_json.len() > MAX_CATALOG_PLAN_JSON_BYTES {
        return Err(EngineError::InvalidCommit(format!(
            "catalog coverage-plan JSON exceeds {MAX_CATALOG_PLAN_JSON_BYTES} bytes"
        )));
    }
    let content_digest = blake3::hash(&plan_json);
    transaction
        .execute(
            r#"
            INSERT INTO catalog_coverage_plans (
                coverage_plan_id, coverage_plan_contract_version, scope_kind,
                plan_json, content_digest, created_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                plan.coverage_plan_id.storage_bytes().as_slice(),
                i64::from(plan.coverage_plan_contract_version),
                LIBRARY_SCOPE,
                plan_json,
                content_digest.as_bytes().as_slice(),
                to_i64(commit_seq, "catalog plan commit")?,
            ],
        )
        .map_err(|error| sqlite_error("insert catalog coverage plan", error))?;
    Ok(())
}

fn insert_partial_coverage(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    expected: &CatalogPartialBuildExpectation,
    prepared: &PreparedCatalogPartialCoverage,
    committed_at: i64,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO catalog_partial_builds (
                partial_commit_seq, predecessor_state_commit_seq,
                coverage_plan_id, readiness_epoch, attempt,
                source_count, encoded_bytes, entries_digest, committed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                to_i64(commit_seq, "catalog partial commit")?,
                to_i64(
                    expected.state_commit_seq,
                    "catalog partial predecessor commit"
                )?,
                expected.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(expected.epoch, "catalog partial epoch")?,
                to_i64(expected.attempt, "catalog partial attempt")?,
                to_i64(
                    prepared.sources.len() as u64,
                    "catalog partial source count"
                )?,
                to_i64(
                    prepared.encoded_bytes as u64,
                    "catalog partial encoded bytes"
                )?,
                prepared.entries_digest.as_slice(),
                committed_at,
            ],
        )
        .map_err(|error| sqlite_error("insert catalog partial-build evidence", error))?;

    let mut statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO catalog_partial_sources (
                partial_commit_seq, ordinal, adapter_id,
                canonical_source_instance_key, payload, payload_digest
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .map_err(|error| sqlite_error("prepare catalog partial-source evidence", error))?;
    for (ordinal, source) in prepared.sources.iter().enumerate() {
        statement
            .execute(params![
                to_i64(commit_seq, "catalog partial commit")?,
                to_i64(ordinal as u64, "catalog partial source ordinal")?,
                source.adapter_id,
                source.source_instance_key.as_slice(),
                source.payload,
                source.payload_digest.as_slice(),
            ])
            .map_err(|error| sqlite_error("insert catalog partial-source evidence", error))?;
    }
    Ok(())
}

fn insert_source_generation_invalidation(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    expected: &CatalogSourceGenerationInvalidationExpectation,
    committed_at: i64,
) -> Result<(), EngineError> {
    let epoch = expected.epoch.checked_add(1).ok_or_else(|| {
        EngineError::InvalidCommit("catalog readiness epoch overflow".to_string())
    })?;
    transaction
        .execute(
            r#"
            INSERT INTO catalog_epoch_invalidations (
                invalidation_commit_seq, predecessor_state_commit_seq,
                coverage_plan_id, previous_epoch, epoch,
                previous_attempt, previous_state,
                retained_snapshot_commit_seq, committed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                to_i64(commit_seq, "catalog epoch invalidation commit")?,
                to_i64(
                    expected.state_commit_seq,
                    "catalog epoch invalidation predecessor"
                )?,
                expected.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(expected.epoch, "catalog previous readiness epoch")?,
                to_i64(epoch, "catalog readiness epoch")?,
                to_i64(expected.attempt, "catalog previous readiness attempt")?,
                expected.state.as_str(),
                expected
                    .last_complete_snapshot
                    .map(|snapshot| {
                        to_i64(
                            snapshot.complete_commit,
                            "catalog invalidation retained snapshot",
                        )
                    })
                    .transpose()?,
                committed_at,
            ],
        )
        .map_err(|error| sqlite_error("insert catalog epoch-invalidation evidence", error))?;
    Ok(())
}

fn exact_source_generation_invalidation_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogSourceGenerationInvalidationExpectation,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let Some(expected_epoch) = expected.epoch.checked_add(1) else {
        return Ok(false);
    };
    if current.plan.coverage_plan_id != expected.coverage_plan_id
        || current.readiness.epoch != expected_epoch
        || current.readiness.attempt != 1
        || current.readiness.state != CatalogReadinessPhase::Building
        || current.readiness.complete_through_commit.is_some()
        || current.readiness.refreshing_from_snapshot.is_some()
        || current.readiness.reason.is_some()
        || !current.readiness.source_coverage.is_empty()
        || current.readiness.last_complete_snapshot != expected.last_complete_snapshot
    {
        return Ok(false);
    }
    let exact: i64 = connection
        .query_row(
            r#"
            SELECT EXISTS(
              SELECT 1
              FROM catalog_epoch_invalidations AS invalidation
              JOIN ingest_commits AS owner
                ON owner.commit_seq = invalidation.invalidation_commit_seq
              WHERE invalidation.invalidation_commit_seq = ?1
                AND invalidation.predecessor_state_commit_seq = ?2
                AND invalidation.coverage_plan_id = ?3
                AND invalidation.previous_epoch = ?4
                AND invalidation.epoch = ?5
                AND invalidation.previous_attempt = ?6
                AND invalidation.previous_state = ?7
                AND invalidation.retained_snapshot_commit_seq IS ?8
                AND invalidation.committed_at = ?9
                AND owner.source_instance_id IS NULL
                AND owner.reason = 'catalog.library.source_generation.invalidated'
                AND owner.started_at = ?10
                AND owner.committed_at = ?9
                AND owner.fact_count = 0
            )
            "#,
            params![
                to_i64(current.last_commit_seq, "catalog epoch invalidation commit")?,
                to_i64(
                    expected.state_commit_seq,
                    "catalog epoch invalidation predecessor"
                )?,
                expected.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(expected.epoch, "catalog previous readiness epoch")?,
                to_i64(expected_epoch, "catalog readiness epoch")?,
                to_i64(expected.attempt, "catalog previous readiness attempt")?,
                expected.state.as_str(),
                expected
                    .last_complete_snapshot
                    .map(|snapshot| {
                        to_i64(
                            snapshot.complete_commit,
                            "catalog invalidation retained snapshot",
                        )
                    })
                    .transpose()?,
                committed_at,
                started_at,
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("load exact catalog epoch invalidation", error))?;
    Ok(exact == 1)
}

fn exact_partial_progress_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogPartialBuildExpectation,
    prepared: &PreparedCatalogPartialCoverage,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let stored = connection
        .query_row(
            r#"
            SELECT partial.predecessor_state_commit_seq,
                   CASE WHEN typeof(partial.coverage_plan_id) = 'blob'
                                  AND length(partial.coverage_plan_id) = 32
                        THEN partial.coverage_plan_id END,
                   partial.readiness_epoch, partial.attempt,
                   partial.source_count, partial.encoded_bytes,
                   CASE WHEN typeof(partial.entries_digest) = 'blob'
                                  AND length(partial.entries_digest) = 32
                        THEN partial.entries_digest END,
                   partial.committed_at,
                   owner.source_instance_id,
                   CASE WHEN typeof(owner.reason) = 'text'
                                  AND owner.reason = 'catalog.library.build.partial'
                        THEN owner.reason END,
                   owner.started_at, owner.committed_at, owner.fact_count
            FROM catalog_partial_builds AS partial
            JOIN ingest_commits AS owner
              ON owner.commit_seq = partial.partial_commit_seq
            WHERE partial.partial_commit_seq = ?1
            "#,
            [to_i64(
                current.last_commit_seq,
                "catalog current partial commit",
            )?],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, i64>(12)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_error("load exact catalog partial progress", error))?;
    let Some((
        predecessor,
        coverage_plan_id,
        epoch,
        attempt,
        source_count,
        encoded_bytes,
        entries_digest,
        evidence_committed_at,
        owner_source,
        owner_reason,
        owner_started_at,
        owner_committed_at,
        owner_fact_count,
    )) = stored
    else {
        return Ok(false);
    };
    Ok(current.readiness.state == CatalogReadinessPhase::Partial
        && current.readiness.scope == expected.scope
        && current.readiness.coverage_plan_id == expected.coverage_plan_id
        && current.readiness.desired_contract_version == expected.desired_contract_version
        && current.readiness.epoch == expected.epoch
        && current.readiness.attempt == expected.attempt
        && positive_u64(predecessor, "catalog partial predecessor commit")?
            == expected.state_commit_seq
        && coverage_plan_id.as_deref()
            == Some(expected.coverage_plan_id.storage_bytes().as_slice())
        && positive_u64(epoch, "catalog partial epoch")? == expected.epoch
        && positive_u64(attempt, "catalog partial attempt")? == expected.attempt
        && usize::try_from(source_count).ok() == Some(prepared.sources.len())
        && usize::try_from(encoded_bytes).ok() == Some(prepared.encoded_bytes)
        && entries_digest.as_deref() == Some(prepared.entries_digest.as_slice())
        && evidence_committed_at == committed_at
        && owner_source.is_none()
        && owner_reason.as_deref() == Some(PARTIAL_REASON)
        && owner_started_at == started_at
        && owner_committed_at == Some(committed_at)
        && owner_fact_count == 0)
}

fn insert_initial_integrity_failure_evidence(
    transaction: &Transaction<'_>,
    failure_commit_seq: u64,
    expected: &CatalogInitialBuildIntegrityExpectation,
    reason_code: &str,
    failed_at: i64,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO catalog_refresh_integrity_failures (
                failure_commit_seq, failed_refresh_commit_seq, coverage_plan_id,
                readiness_epoch, attempt, retained_snapshot_commit_seq,
                retained_publication_digest, retained_content_digest,
                reason_code, snapshot_disposition, failed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL, ?6, 'discarded', ?7)
            "#,
            params![
                to_i64(
                    failure_commit_seq,
                    "catalog initial integrity-failure commit"
                )?,
                to_i64(
                    expected.build_started_commit_seq,
                    "catalog failed initial-build commit",
                )?,
                expected.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(expected.epoch, "catalog readiness epoch")?,
                to_i64(expected.attempt, "catalog readiness attempt")?,
                reason_code,
                failed_at,
            ],
        )
        .map_err(|error| {
            sqlite_error("insert initial catalog integrity-failure evidence", error)
        })?;
    Ok(())
}

fn insert_initial_source_failure_evidence(
    transaction: &Transaction<'_>,
    failure_commit_seq: u64,
    expected: &CatalogInitialBuildSourceExpectation,
    reason_code: &str,
    failed_at: i64,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO catalog_initial_source_failures (
                failure_commit_seq, failed_build_commit_seq, coverage_plan_id,
                readiness_epoch, attempt, previous_state, reason_code, failed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                to_i64(failure_commit_seq, "catalog initial source-failure commit")?,
                to_i64(
                    expected.build_state_commit_seq,
                    "catalog failed initial-source build commit",
                )?,
                expected.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(expected.epoch, "catalog readiness epoch")?,
                to_i64(expected.attempt, "catalog readiness attempt")?,
                expected.state.as_str(),
                reason_code,
                failed_at,
            ],
        )
        .map_err(|error| sqlite_error("insert initial catalog source-failure evidence", error))?;
    Ok(())
}

struct StoredCatalogInitialSourceFailure {
    failure_commit_seq: i64,
    failed_build_commit_seq: i64,
    coverage_plan_id: Option<Vec<u8>>,
    readiness_epoch: i64,
    attempt: i64,
    previous_state: Option<String>,
    reason_code: Option<String>,
    failed_at: i64,
    failure_source: Option<i64>,
    failure_reason: Option<String>,
    failure_started_at: i64,
    failure_committed_at: Option<i64>,
    failure_fact_count: i64,
    build_source: Option<i64>,
    build_reason: Option<String>,
    build_committed_at: Option<i64>,
    build_fact_count: i64,
}

fn load_latest_initial_source_failure_evidence(
    connection: &Connection,
) -> Result<Option<StoredCatalogInitialSourceFailure>, EngineError> {
    validate_initial_source_failure_ledger(connection)?;
    connection
        .query_row(
            r#"
            SELECT f.failure_commit_seq,
                   f.failed_build_commit_seq,
                   CASE WHEN typeof(f.coverage_plan_id) = 'blob'
                                  AND length(f.coverage_plan_id) = 32
                        THEN f.coverage_plan_id END,
                   f.readiness_epoch,
                   f.attempt,
                   CASE WHEN typeof(f.previous_state) = 'text'
                                  AND f.previous_state IN ('building', 'partial')
                        THEN f.previous_state END,
                   CASE WHEN typeof(f.reason_code) = 'text'
                                  AND length(CAST(f.reason_code AS BLOB)) BETWEEN 1 AND 64
                                  AND length(f.reason_code) = length(CAST(f.reason_code AS BLOB))
                                  AND substr(f.reason_code, 1, 1) GLOB '[a-z]'
                                  AND f.reason_code NOT GLOB '*[^a-z0-9_]*'
                        THEN f.reason_code END,
                   f.failed_at,
                   failed.source_instance_id,
                   CASE WHEN typeof(failed.reason) = 'text'
                                  AND failed.reason = 'catalog.library.build.source_unavailable'
                        THEN failed.reason END,
                   failed.started_at,
                   failed.committed_at,
                   failed.fact_count,
                   build.source_instance_id,
                   CASE WHEN typeof(build.reason) = 'text'
                                  AND build.reason IN (
                                      'catalog.library.build.scheduled',
                                      'catalog.library.build.partial',
                                      'catalog.library.build.source_retrying',
                                      'catalog.library.refresh.recovery_started',
                                      'catalog.library.source_generation.invalidated'
                                  )
                        THEN build.reason END,
                   build.committed_at,
                   build.fact_count
            FROM catalog_initial_source_failures AS f
            LEFT JOIN ingest_commits AS failed
              ON failed.commit_seq = f.failure_commit_seq
            LEFT JOIN ingest_commits AS build
              ON build.commit_seq = f.failed_build_commit_seq
            ORDER BY f.failure_commit_seq DESC
            LIMIT 1
            "#,
            [],
            |row| {
                Ok(StoredCatalogInitialSourceFailure {
                    failure_commit_seq: row.get(0)?,
                    failed_build_commit_seq: row.get(1)?,
                    coverage_plan_id: row.get(2)?,
                    readiness_epoch: row.get(3)?,
                    attempt: row.get(4)?,
                    previous_state: row.get(5)?,
                    reason_code: row.get(6)?,
                    failed_at: row.get(7)?,
                    failure_source: row.get(8)?,
                    failure_reason: row.get(9)?,
                    failure_started_at: row.get(10)?,
                    failure_committed_at: row.get(11)?,
                    failure_fact_count: row.get(12)?,
                    build_source: row.get(13)?,
                    build_reason: row.get(14)?,
                    build_committed_at: row.get(15)?,
                    build_fact_count: row.get(16)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_error("load initial catalog source-failure evidence", error))
}

fn validate_initial_source_failure_ledger(connection: &Connection) -> Result<(), EngineError> {
    let invalid: i64 = connection
        .query_row(
            r#"
            SELECT COUNT(*)
            FROM catalog_initial_source_failures AS f
            LEFT JOIN ingest_commits AS failed
              ON failed.commit_seq = f.failure_commit_seq
            LEFT JOIN ingest_commits AS build
              ON build.commit_seq = f.failed_build_commit_seq
            WHERE failed.commit_seq IS NULL
               OR build.commit_seq IS NULL
               OR failed.source_instance_id IS NOT NULL
               OR failed.reason != 'catalog.library.build.source_unavailable'
               OR failed.committed_at IS NULL
               OR failed.committed_at != f.failed_at
               OR failed.started_at > failed.committed_at
               OR failed.fact_count != 0
               OR build.source_instance_id IS NOT NULL
               OR build.committed_at IS NULL
               OR build.fact_count != 0
               OR f.failed_build_commit_seq >= f.failure_commit_seq
               OR typeof(f.coverage_plan_id) != 'blob'
               OR length(f.coverage_plan_id) != 32
               OR f.readiness_epoch <= 0
               OR f.attempt <= 0
               OR f.previous_state NOT IN ('building', 'partial')
               OR typeof(f.reason_code) != 'text'
               OR length(CAST(f.reason_code AS BLOB)) NOT BETWEEN 1 AND 64
               OR length(f.reason_code) != length(CAST(f.reason_code AS BLOB))
               OR substr(f.reason_code, 1, 1) NOT GLOB '[a-z]'
               OR f.reason_code GLOB '*[^a-z0-9_]*'
               OR build.reason NOT IN (
                    'catalog.library.build.scheduled',
                    'catalog.library.build.partial',
                    'catalog.library.build.source_retrying',
                    'catalog.library.refresh.recovery_started',
                    'catalog.library.source_generation.invalidated'
               )
               OR (f.previous_state = 'partial'
                   AND build.reason NOT IN (
                       'catalog.library.build.partial',
                       'catalog.library.build.source_retrying'
                   ))
            "#,
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate initial catalog source-failure ledger", error))?;
    let duplicate_attempts: i64 = connection
        .query_row(
            r#"
            SELECT COUNT(*) FROM (
              SELECT coverage_plan_id, readiness_epoch, attempt
              FROM catalog_initial_source_failures
              GROUP BY coverage_plan_id, readiness_epoch, attempt
              HAVING COUNT(*) != 1
            )
            "#,
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate initial catalog source-failure attempts", error))?;
    if invalid != 0 || duplicate_attempts != 0 {
        return Err(corrupt_catalog_state(
            "initial catalog source-failure ledger is not one canonical sequence of terminal attempts",
        ));
    }
    Ok(())
}

fn insert_integrity_failure_evidence(
    transaction: &Transaction<'_>,
    failure_commit_seq: u64,
    expected: &CatalogActiveRefreshPublicationExpectation,
    reason_code: &str,
    failed_at: i64,
) -> Result<(), EngineError> {
    let retained = expected
        .publication_identity
        .retained_chain()
        .last()
        .copied()
        .ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog integrity failure has no retained publication commitment".to_string(),
            )
        })?;
    if retained.snapshot_id() != expected.predecessor_snapshot {
        return Err(EngineError::InvalidCommit(
            "catalog integrity failure retained commitment differs from its predecessor"
                .to_string(),
        ));
    }
    transaction
        .execute(
            r#"
            INSERT INTO catalog_refresh_integrity_failures (
                failure_commit_seq, failed_refresh_commit_seq, coverage_plan_id,
                readiness_epoch, attempt, retained_snapshot_commit_seq,
                retained_publication_digest, retained_content_digest,
                reason_code, snapshot_disposition, failed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'independently_safe', ?10)
            "#,
            params![
                to_i64(failure_commit_seq, "catalog integrity-failure commit")?,
                to_i64(
                    expected.refresh_started_commit_seq,
                    "catalog failed refresh commit",
                )?,
                expected.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(expected.epoch, "catalog readiness epoch")?,
                to_i64(expected.attempt, "catalog readiness attempt")?,
                to_i64(
                    expected.predecessor_snapshot.complete_commit,
                    "catalog retained snapshot commit",
                )?,
                retained.publication_digest().as_slice(),
                retained.content_digest().as_slice(),
                reason_code,
                failed_at,
            ],
        )
        .map_err(|error| sqlite_error("insert catalog integrity-failure evidence", error))?;
    Ok(())
}

fn insert_source_failure_evidence(
    transaction: &Transaction<'_>,
    failure_commit_seq: u64,
    expected: &CatalogActiveRefreshPublicationExpectation,
    reason_code: &str,
    failed_at: i64,
) -> Result<(), EngineError> {
    let retained = expected
        .publication_identity
        .retained_chain()
        .last()
        .copied()
        .ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog source failure has no retained publication commitment".to_string(),
            )
        })?;
    if retained.snapshot_id() != expected.predecessor_snapshot {
        return Err(EngineError::InvalidCommit(
            "catalog source failure retained commitment differs from its predecessor".to_string(),
        ));
    }
    transaction
        .execute(
            r#"
            INSERT INTO catalog_refresh_source_failures (
                failure_commit_seq, failed_refresh_commit_seq, coverage_plan_id,
                readiness_epoch, attempt, retained_snapshot_commit_seq,
                retained_publication_digest, retained_content_digest,
                reason_code, failed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                to_i64(failure_commit_seq, "catalog source-failure commit")?,
                to_i64(
                    expected.refresh_started_commit_seq,
                    "catalog unavailable refresh commit",
                )?,
                expected.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(expected.epoch, "catalog readiness epoch")?,
                to_i64(expected.attempt, "catalog readiness attempt")?,
                to_i64(
                    expected.predecessor_snapshot.complete_commit,
                    "catalog retained snapshot commit",
                )?,
                retained.publication_digest().as_slice(),
                retained.content_digest().as_slice(),
                reason_code,
                failed_at,
            ],
        )
        .map_err(|error| sqlite_error("insert catalog source-failure evidence", error))?;
    Ok(())
}

struct StoredCatalogSourceFailure {
    failure_commit_seq: i64,
    failed_refresh_commit_seq: i64,
    coverage_plan_id: Option<Vec<u8>>,
    readiness_epoch: i64,
    attempt: i64,
    retained_snapshot_commit_seq: i64,
    retained_publication_digest: Option<Vec<u8>>,
    retained_content_digest: Option<Vec<u8>>,
    reason_code: Option<String>,
    failed_at: i64,
    failure_source: Option<i64>,
    failure_reason: Option<String>,
    failure_started_at: i64,
    failure_committed_at: Option<i64>,
    failure_fact_count: i64,
    refresh_source: Option<i64>,
    refresh_reason: Option<String>,
    refresh_committed_at: Option<i64>,
    refresh_fact_count: i64,
}

fn load_latest_source_failure_evidence(
    connection: &Connection,
) -> Result<Option<StoredCatalogSourceFailure>, EngineError> {
    validate_source_failure_ledger(connection)?;
    connection
        .query_row(
            r#"
            SELECT f.failure_commit_seq,
                   f.failed_refresh_commit_seq,
                   CASE WHEN typeof(f.coverage_plan_id) = 'blob'
                                  AND length(f.coverage_plan_id) = 32
                        THEN f.coverage_plan_id END,
                   f.readiness_epoch,
                   f.attempt,
                   f.retained_snapshot_commit_seq,
                   CASE WHEN typeof(f.retained_publication_digest) = 'blob'
                                  AND length(f.retained_publication_digest) = 32
                        THEN f.retained_publication_digest END,
                   CASE WHEN typeof(f.retained_content_digest) = 'blob'
                                  AND length(f.retained_content_digest) = 32
                        THEN f.retained_content_digest END,
                   CASE WHEN typeof(f.reason_code) = 'text'
                                  AND length(CAST(f.reason_code AS BLOB)) BETWEEN 1 AND 64
                                  AND length(f.reason_code) = length(CAST(f.reason_code AS BLOB))
                                  AND substr(f.reason_code, 1, 1) GLOB '[a-z]'
                                  AND f.reason_code NOT GLOB '*[^a-z0-9_]*'
                        THEN f.reason_code END,
                   f.failed_at,
                   failed.source_instance_id,
                   CASE WHEN typeof(failed.reason) = 'text'
                                  AND failed.reason = 'catalog.library.refresh.source_unavailable'
                        THEN failed.reason END,
                   failed.started_at,
                   failed.committed_at,
                   failed.fact_count,
                   refresh.source_instance_id,
                   CASE WHEN typeof(refresh.reason) = 'text'
                                  AND refresh.reason IN (
                                      'catalog.library.refresh.started',
                                      'catalog.library.refresh.source_retrying',
                                      'catalog.library.refresh.recovery_started',
                                      'catalog.library.source_generation.invalidated',
                                      'catalog.library.build.partial'
                                  )
                        THEN refresh.reason END,
                   refresh.committed_at,
                   refresh.fact_count
            FROM catalog_refresh_source_failures AS f
            LEFT JOIN ingest_commits AS failed
              ON failed.commit_seq = f.failure_commit_seq
            LEFT JOIN ingest_commits AS refresh
              ON refresh.commit_seq = f.failed_refresh_commit_seq
            ORDER BY f.failure_commit_seq DESC
            LIMIT 1
            "#,
            [],
            |row| {
                Ok(StoredCatalogSourceFailure {
                    failure_commit_seq: row.get(0)?,
                    failed_refresh_commit_seq: row.get(1)?,
                    coverage_plan_id: row.get(2)?,
                    readiness_epoch: row.get(3)?,
                    attempt: row.get(4)?,
                    retained_snapshot_commit_seq: row.get(5)?,
                    retained_publication_digest: row.get(6)?,
                    retained_content_digest: row.get(7)?,
                    reason_code: row.get(8)?,
                    failed_at: row.get(9)?,
                    failure_source: row.get(10)?,
                    failure_reason: row.get(11)?,
                    failure_started_at: row.get(12)?,
                    failure_committed_at: row.get(13)?,
                    failure_fact_count: row.get(14)?,
                    refresh_source: row.get(15)?,
                    refresh_reason: row.get(16)?,
                    refresh_committed_at: row.get(17)?,
                    refresh_fact_count: row.get(18)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_error("load catalog source-failure evidence", error))
}

fn validate_source_failure_ledger(connection: &Connection) -> Result<(), EngineError> {
    let invalid: i64 = connection
        .query_row(
            r#"
            SELECT COUNT(*)
            FROM catalog_refresh_source_failures AS f
            LEFT JOIN ingest_commits AS failed
              ON failed.commit_seq = f.failure_commit_seq
            LEFT JOIN ingest_commits AS refresh
              ON refresh.commit_seq = f.failed_refresh_commit_seq
            LEFT JOIN catalog_snapshots AS retained
              ON retained.snapshot_commit_seq = f.retained_snapshot_commit_seq
            WHERE failed.commit_seq IS NULL
               OR refresh.commit_seq IS NULL
               OR retained.snapshot_commit_seq IS NULL
               OR failed.source_instance_id IS NOT NULL
               OR failed.reason != 'catalog.library.refresh.source_unavailable'
               OR failed.committed_at IS NULL
               OR failed.committed_at != f.failed_at
               OR failed.started_at > failed.committed_at
               OR failed.fact_count != 0
               OR refresh.source_instance_id IS NOT NULL
               OR refresh.committed_at IS NULL
               OR refresh.fact_count != 0
               OR COALESCE((
                    (f.readiness_epoch = retained.readiness_epoch AND (
                      (f.attempt = retained.attempt
                       AND refresh.reason IN (
                         'catalog.library.refresh.started',
                         'catalog.library.refresh.source_retrying'
                       ))
                      OR
                      (f.attempt > retained.attempt
                       AND refresh.reason IN (
                         'catalog.library.refresh.recovery_started',
                         'catalog.library.refresh.source_retrying',
                         'catalog.library.build.partial'
                       ))
                    ))
                    OR
                    (f.readiness_epoch > retained.readiness_epoch AND (
                      (f.attempt = 1
                       AND refresh.reason IN (
                         'catalog.library.source_generation.invalidated',
                         'catalog.library.refresh.source_retrying',
                         'catalog.library.build.partial'
                       ))
                      OR
                      (f.attempt > 1
                       AND refresh.reason IN (
                         'catalog.library.refresh.recovery_started',
                         'catalog.library.refresh.source_retrying',
                         'catalog.library.build.partial'
                       ))
                    ))
               ), 0) = 0
               OR f.coverage_plan_id != retained.coverage_plan_id
               OR f.readiness_epoch < retained.readiness_epoch
               OR f.retained_publication_digest != retained.publication_digest
               OR f.retained_content_digest != retained.content_digest
               OR typeof(f.reason_code) != 'text'
               OR length(CAST(f.reason_code AS BLOB)) NOT BETWEEN 1 AND 64
               OR length(f.reason_code) != length(CAST(f.reason_code AS BLOB))
               OR substr(f.reason_code, 1, 1) NOT GLOB '[a-z]'
               OR f.reason_code GLOB '*[^a-z0-9_]*'
               OR COALESCE((
                    f.retained_snapshot_commit_seq < f.failed_refresh_commit_seq
                    AND f.failed_refresh_commit_seq < f.failure_commit_seq
               ), 0) = 0
            "#,
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate catalog source-failure ledger", error))?;
    let duplicate_attempts: i64 = connection
        .query_row(
            r#"
            SELECT COUNT(*) FROM (
              SELECT coverage_plan_id, readiness_epoch, attempt
              FROM catalog_refresh_source_failures
              GROUP BY coverage_plan_id, readiness_epoch, attempt
              HAVING COUNT(*) != 1
            )
            "#,
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate catalog source-failure attempts", error))?;
    if invalid != 0 || duplicate_attempts != 0 {
        return Err(corrupt_catalog_state(
            "catalog source-failure ledger is not one canonical sequence of terminal attempts",
        ));
    }
    Ok(())
}

struct StoredCatalogIntegrityFailure {
    failure_commit_seq: i64,
    failed_refresh_commit_seq: i64,
    coverage_plan_id: Option<Vec<u8>>,
    readiness_epoch: i64,
    attempt: i64,
    retained_snapshot_commit_seq: Option<i64>,
    retained_publication_digest: Option<Vec<u8>>,
    retained_content_digest: Option<Vec<u8>>,
    reason_code: Option<String>,
    snapshot_disposition: Option<String>,
    failed_at: i64,
    failure_source: Option<i64>,
    failure_reason: Option<String>,
    failure_started_at: i64,
    failure_committed_at: Option<i64>,
    failure_fact_count: i64,
    refresh_source: Option<i64>,
    refresh_reason: Option<String>,
    refresh_committed_at: Option<i64>,
    refresh_fact_count: i64,
}

fn load_latest_integrity_failure_evidence(
    connection: &Connection,
) -> Result<Option<StoredCatalogIntegrityFailure>, EngineError> {
    validate_integrity_failure_ledger(connection)?;
    connection
        .query_row(
            r#"
            SELECT f.failure_commit_seq,
                   f.failed_refresh_commit_seq,
                   CASE WHEN typeof(f.coverage_plan_id) = 'blob'
                                  AND length(f.coverage_plan_id) = 32
                        THEN f.coverage_plan_id END,
                   f.readiness_epoch,
                   f.attempt,
                   CASE WHEN f.retained_snapshot_commit_seq IS NULL THEN NULL
                        WHEN typeof(f.retained_snapshot_commit_seq) = 'integer'
                                  AND f.retained_snapshot_commit_seq > 0
                        THEN f.retained_snapshot_commit_seq END,
                   CASE WHEN typeof(f.retained_publication_digest) = 'blob'
                                  AND length(f.retained_publication_digest) = 32
                        THEN f.retained_publication_digest END,
                   CASE WHEN typeof(f.retained_content_digest) = 'blob'
                                  AND length(f.retained_content_digest) = 32
                        THEN f.retained_content_digest END,
                   CASE WHEN typeof(f.reason_code) = 'text'
                                  AND length(CAST(f.reason_code AS BLOB)) BETWEEN 1 AND 64
                                  AND length(f.reason_code) = length(CAST(f.reason_code AS BLOB))
                                  AND substr(f.reason_code, 1, 1) GLOB '[a-z]'
                                  AND f.reason_code NOT GLOB '*[^a-z0-9_]*'
                        THEN f.reason_code END,
                   CASE WHEN typeof(f.snapshot_disposition) = 'text'
                                  AND f.snapshot_disposition IN ('independently_safe', 'discarded')
                        THEN f.snapshot_disposition END,
                   f.failed_at,
                   failed.source_instance_id,
                   CASE WHEN typeof(failed.reason) = 'text'
                                  AND failed.reason IN (
                                      'catalog.library.build.integrity_failed',
                                      'catalog.library.refresh.integrity_failed'
                                  )
                        THEN failed.reason END,
                   failed.started_at,
                   failed.committed_at,
                   failed.fact_count,
                   refresh.source_instance_id,
                   CASE WHEN typeof(refresh.reason) = 'text'
                                  AND refresh.reason IN (
                                      'catalog.library.build.scheduled',
                                      'catalog.library.build.partial',
                                      'catalog.library.build.source_retrying',
                                      'catalog.library.refresh.started',
                                      'catalog.library.refresh.source_retrying',
                                      'catalog.library.refresh.recovery_started',
                                      'catalog.library.source_generation.invalidated'
                                  )
                        THEN refresh.reason END,
                   refresh.committed_at,
                   refresh.fact_count
            FROM catalog_refresh_integrity_failures AS f
            LEFT JOIN ingest_commits AS failed
              ON failed.commit_seq = f.failure_commit_seq
            LEFT JOIN ingest_commits AS refresh
              ON refresh.commit_seq = f.failed_refresh_commit_seq
            ORDER BY f.failure_commit_seq DESC
            LIMIT 1
            "#,
            [],
            |row| {
                Ok(StoredCatalogIntegrityFailure {
                    failure_commit_seq: row.get(0)?,
                    failed_refresh_commit_seq: row.get(1)?,
                    coverage_plan_id: row.get(2)?,
                    readiness_epoch: row.get(3)?,
                    attempt: row.get(4)?,
                    retained_snapshot_commit_seq: row.get(5)?,
                    retained_publication_digest: row.get(6)?,
                    retained_content_digest: row.get(7)?,
                    reason_code: row.get(8)?,
                    snapshot_disposition: row.get(9)?,
                    failed_at: row.get(10)?,
                    failure_source: row.get(11)?,
                    failure_reason: row.get(12)?,
                    failure_started_at: row.get(13)?,
                    failure_committed_at: row.get(14)?,
                    failure_fact_count: row.get(15)?,
                    refresh_source: row.get(16)?,
                    refresh_reason: row.get(17)?,
                    refresh_committed_at: row.get(18)?,
                    refresh_fact_count: row.get(19)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_error("load catalog integrity-failure evidence", error))
}

fn validate_integrity_failure_ledger(connection: &Connection) -> Result<(), EngineError> {
    let invalid: i64 = connection
        .query_row(
            r#"
            SELECT COUNT(*)
            FROM catalog_refresh_integrity_failures AS f
            LEFT JOIN ingest_commits AS failed
              ON failed.commit_seq = f.failure_commit_seq
            LEFT JOIN ingest_commits AS refresh
              ON refresh.commit_seq = f.failed_refresh_commit_seq
            LEFT JOIN catalog_snapshots AS retained
              ON retained.snapshot_commit_seq = f.retained_snapshot_commit_seq
            WHERE failed.commit_seq IS NULL
               OR refresh.commit_seq IS NULL
               OR failed.source_instance_id IS NOT NULL
               OR failed.committed_at IS NULL
               OR failed.committed_at != f.failed_at
               OR failed.started_at > failed.committed_at
               OR failed.fact_count != 0
               OR refresh.source_instance_id IS NOT NULL
               OR refresh.committed_at IS NULL
               OR refresh.fact_count != 0
               OR typeof(f.reason_code) != 'text'
               OR length(CAST(f.reason_code AS BLOB)) NOT BETWEEN 1 AND 64
               OR length(f.reason_code) != length(CAST(f.reason_code AS BLOB))
               OR substr(f.reason_code, 1, 1) NOT GLOB '[a-z]'
               OR f.reason_code GLOB '*[^a-z0-9_]*'
               OR COALESCE((
                    (
                      f.snapshot_disposition = 'independently_safe'
                      AND failed.reason = 'catalog.library.refresh.integrity_failed'
                      AND retained.snapshot_commit_seq IS NOT NULL
                      AND f.retained_snapshot_commit_seq < f.failed_refresh_commit_seq
                      AND f.failed_refresh_commit_seq < f.failure_commit_seq
                      AND (
                        (f.readiness_epoch = retained.readiness_epoch AND (
                          (f.attempt = retained.attempt
                           AND refresh.reason IN (
                             'catalog.library.refresh.started',
                             'catalog.library.refresh.source_retrying'
                           ))
                          OR
                          (f.attempt > retained.attempt
                           AND refresh.reason IN (
                             'catalog.library.refresh.recovery_started',
                             'catalog.library.refresh.source_retrying',
                             'catalog.library.build.partial'
                           ))
                        ))
                        OR
                        (f.readiness_epoch > retained.readiness_epoch AND (
                          (f.attempt = 1
                           AND refresh.reason IN (
                             'catalog.library.source_generation.invalidated',
                             'catalog.library.refresh.source_retrying',
                             'catalog.library.build.partial'
                           ))
                          OR
                          (f.attempt > 1
                           AND refresh.reason IN (
                             'catalog.library.refresh.recovery_started',
                             'catalog.library.refresh.source_retrying',
                             'catalog.library.build.partial'
                           ))
                        ))
                      )
                      AND f.coverage_plan_id = retained.coverage_plan_id
                      AND f.readiness_epoch >= retained.readiness_epoch
                      AND f.retained_publication_digest = retained.publication_digest
                      AND f.retained_content_digest = retained.content_digest
                    )
                    OR
                    (
                      f.snapshot_disposition = 'discarded'
                      AND failed.reason = 'catalog.library.build.integrity_failed'
                      AND f.retained_snapshot_commit_seq IS NULL
                      AND f.retained_publication_digest IS NULL
                      AND f.retained_content_digest IS NULL
                      AND retained.snapshot_commit_seq IS NULL
                      AND f.failed_refresh_commit_seq < f.failure_commit_seq
                      AND (
                        (f.readiness_epoch = 1
                         AND f.attempt = 1
                         AND refresh.reason IN (
                           'catalog.library.build.scheduled',
                           'catalog.library.build.partial',
                           'catalog.library.build.source_retrying'
                         ))
                        OR
                        (f.readiness_epoch > 1
                         AND f.attempt = 1
                         AND refresh.reason IN (
                           'catalog.library.source_generation.invalidated',
                           'catalog.library.build.partial',
                           'catalog.library.build.source_retrying'
                         )
                         AND EXISTS(
                           SELECT 1 FROM catalog_epoch_invalidations AS invalidation
                           WHERE invalidation.invalidation_commit_seq <= f.failed_refresh_commit_seq
                             AND invalidation.coverage_plan_id = f.coverage_plan_id
                             AND invalidation.epoch = f.readiness_epoch
                             AND invalidation.retained_snapshot_commit_seq IS NULL
                         ))
                        OR
                        (f.attempt > 1
                         AND refresh.reason IN (
                           'catalog.library.refresh.recovery_started',
                           'catalog.library.build.partial',
                           'catalog.library.build.source_retrying'
                         ))
                      )
                    )
               ), 0) = 0
            "#,
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate catalog integrity-failure ledger", error))?;
    let duplicate_attempts: i64 = connection
        .query_row(
            r#"
            SELECT COUNT(*) FROM (
              SELECT coverage_plan_id, readiness_epoch, attempt
              FROM catalog_refresh_integrity_failures
              GROUP BY coverage_plan_id, readiness_epoch, attempt
              HAVING COUNT(*) != 1
            )
            "#,
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("validate catalog integrity-failure attempts", error))?;
    if invalid != 0 || duplicate_attempts != 0 {
        return Err(corrupt_catalog_state(
            "catalog integrity-failure ledger is not one canonical sequence of terminal attempts",
        ));
    }
    Ok(())
}

fn exact_integrity_failure_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogActiveRefreshPublicationExpectation,
    reason_code: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let Some(stored) = load_latest_integrity_failure_evidence(connection)? else {
        return Ok(false);
    };
    let Some(retained) = expected
        .publication_identity
        .retained_chain()
        .last()
        .copied()
    else {
        return Ok(false);
    };
    let Some(retained_snapshot_commit_seq) = stored.retained_snapshot_commit_seq else {
        return Ok(false);
    };
    Ok(current.readiness.state == CatalogReadinessPhase::Error
        && current.readiness.last_complete_snapshot == Some(expected.predecessor_snapshot)
        && current.readiness.complete_through_commit
            == (!expected.is_epoch_replacement())
                .then_some(expected.predecessor_snapshot.complete_commit)
        && current.readiness.reason
            == Some(CatalogReadinessReason::IntegrityFailure {
                code: reason_code.to_string(),
                snapshot_disposition: CatalogIntegritySnapshotDisposition::IndependentlySafe,
            })
        && current.last_commit_seq
            == positive_u64(
                stored.failure_commit_seq,
                "catalog integrity-failure commit",
            )?
        && positive_u64(
            stored.failed_refresh_commit_seq,
            "catalog failed refresh commit",
        )? == expected.refresh_started_commit_seq
        && stored.coverage_plan_id.as_deref()
            == Some(expected.coverage_plan_id.storage_bytes().as_slice())
        && positive_u64(stored.readiness_epoch, "catalog failure epoch")? == expected.epoch
        && positive_u64(stored.attempt, "catalog failure attempt")? == expected.attempt
        && positive_u64(
            retained_snapshot_commit_seq,
            "catalog failure retained snapshot",
        )? == expected.predecessor_snapshot.complete_commit
        && stored.retained_publication_digest.as_deref()
            == Some(retained.publication_digest().as_slice())
        && stored.retained_content_digest.as_deref() == Some(retained.content_digest().as_slice())
        && stored.reason_code.as_deref() == Some(reason_code)
        && stored.snapshot_disposition.as_deref() == Some("independently_safe")
        && stored.failed_at == committed_at
        && stored.failure_source.is_none()
        && stored.failure_reason.as_deref() == Some(REFRESH_INTEGRITY_FAILURE_REASON)
        && stored.failure_started_at == started_at
        && stored.failure_committed_at == Some(committed_at)
        && stored.failure_fact_count == 0
        && stored.refresh_source.is_none()
        && stored.refresh_reason.as_deref() == Some(refresh_execution_reason(expected))
        && stored.refresh_committed_at.is_some()
        && stored.refresh_fact_count == 0)
}

fn initial_build_execution_reason(
    epoch: u64,
    attempt: u64,
    state: CatalogDurableBuildPhase,
) -> &'static str {
    if state == CatalogDurableBuildPhase::Partial {
        PARTIAL_REASON
    } else if attempt > 1 {
        REFRESH_RECOVERY_STARTED_REASON
    } else if epoch > 1 {
        SOURCE_GENERATION_INVALIDATED_REASON
    } else {
        SCHEDULE_REASON
    }
}

fn exact_initial_integrity_failure_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogInitialBuildIntegrityExpectation,
    reason_code: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let Some(stored) = load_latest_integrity_failure_evidence(connection)? else {
        return Ok(false);
    };
    let expected_execution_reason = if expected.retry_reason_code.is_some() {
        INITIAL_SOURCE_RETRYING_REASON
    } else {
        initial_build_execution_reason(expected.epoch, expected.attempt, expected.state)
    };
    Ok(current.readiness.state == CatalogReadinessPhase::Error
        && current.readiness.scope == expected.scope
        && current.readiness.coverage_plan_id == expected.coverage_plan_id
        && current.readiness.desired_contract_version == expected.desired_contract_version
        && current.readiness.epoch == expected.epoch
        && current.readiness.attempt == expected.attempt
        && current.readiness.completed_contract_version.is_none()
        && current.readiness.complete_through_commit.is_none()
        && current.readiness.last_complete_snapshot.is_none()
        && current.readiness.refreshing_from_snapshot.is_none()
        && current.readiness.source_coverage.is_empty()
        && current.readiness.reason
            == Some(CatalogReadinessReason::IntegrityFailure {
                code: reason_code.to_string(),
                snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
            })
        && current.last_commit_seq
            == positive_u64(
                stored.failure_commit_seq,
                "catalog initial integrity-failure commit",
            )?
        && positive_u64(
            stored.failed_refresh_commit_seq,
            "catalog failed initial-build commit",
        )? == expected.build_started_commit_seq
        && stored.coverage_plan_id.as_deref()
            == Some(expected.coverage_plan_id.storage_bytes().as_slice())
        && positive_u64(stored.readiness_epoch, "catalog initial failure epoch")? == expected.epoch
        && positive_u64(stored.attempt, "catalog initial failure attempt")? == expected.attempt
        && stored.retained_snapshot_commit_seq.is_none()
        && stored.retained_publication_digest.is_none()
        && stored.retained_content_digest.is_none()
        && stored.reason_code.as_deref() == Some(reason_code)
        && stored.snapshot_disposition.as_deref() == Some("discarded")
        && stored.failed_at == committed_at
        && stored.failure_source.is_none()
        && stored.failure_reason.as_deref() == Some(INITIAL_INTEGRITY_FAILURE_REASON)
        && stored.failure_started_at == started_at
        && stored.failure_committed_at == Some(committed_at)
        && stored.failure_fact_count == 0
        && stored.refresh_source.is_none()
        && stored.refresh_reason.as_deref() == Some(expected_execution_reason)
        && stored.refresh_committed_at.is_some()
        && stored.refresh_fact_count == 0)
}

fn exact_initial_source_retry_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogInitialBuildSourceExpectation,
    reason_code: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let actual = current.initial_source_expectation()?;
    let (source, reason, stored_started_at, stored_committed_at, fact_count): (
        Option<i64>,
        String,
        i64,
        Option<i64>,
        i64,
    ) = connection
        .query_row(
            r#"
            SELECT source_instance_id, reason, started_at, committed_at, fact_count
            FROM ingest_commits WHERE commit_seq = ?1
            "#,
            [to_i64(
                current.last_commit_seq,
                "catalog initial source-retry commit",
            )?],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|error| sqlite_error("load initial catalog source-retry commit", error))?;
    let change_matches = validate_initial_source_change(
        connection,
        current.last_commit_seq,
        &current.plan,
        current.readiness.desired_contract_version,
        current.readiness.epoch,
        current.readiness.attempt,
        expected.state,
        expected.state.readiness_phase(),
        "source_retrying",
        current.readiness.source_coverage.len(),
        Some(reason_code),
    )? == expected.build_state_commit_seq;
    Ok(change_matches
        && actual.scope == expected.scope
        && actual.coverage_plan_id == expected.coverage_plan_id
        && actual.desired_contract_version == expected.desired_contract_version
        && actual.epoch == expected.epoch
        && actual.attempt == expected.attempt
        && actual.state == expected.state
        && actual.retry_reason_code.as_deref() == Some(reason_code)
        && actual.build_state_commit_seq > expected.build_state_commit_seq
        && source.is_none()
        && reason == INITIAL_SOURCE_RETRYING_REASON
        && stored_started_at == started_at
        && stored_committed_at == Some(committed_at)
        && fact_count == 0)
}

fn exact_initial_source_failure_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogInitialBuildSourceExpectation,
    reason_code: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let Some(stored) = load_latest_initial_source_failure_evidence(connection)? else {
        return Ok(false);
    };
    let change_matches = validate_initial_source_change(
        connection,
        current.last_commit_seq,
        &current.plan,
        current.readiness.desired_contract_version,
        current.readiness.epoch,
        current.readiness.attempt,
        expected.state,
        CatalogReadinessPhase::Degraded,
        "source_unavailable",
        current.readiness.source_coverage.len(),
        Some(reason_code),
    )? == expected.build_state_commit_seq;
    Ok(change_matches
        && current.readiness.state == CatalogReadinessPhase::Degraded
        && current.readiness.scope == expected.scope
        && current.readiness.coverage_plan_id == expected.coverage_plan_id
        && current.readiness.desired_contract_version == expected.desired_contract_version
        && current.readiness.epoch == expected.epoch
        && current.readiness.attempt == expected.attempt
        && current.readiness.completed_contract_version.is_none()
        && current.readiness.complete_through_commit.is_none()
        && current.readiness.last_complete_snapshot.is_none()
        && current.readiness.refreshing_from_snapshot.is_none()
        && current
            .plan
            .required_coverage_present(&current.readiness.source_coverage)
        && current.readiness.reason
            == Some(CatalogReadinessReason::TerminalSourceUnavailable {
                code: reason_code.to_string(),
            })
        && current.last_commit_seq
            == positive_u64(
                stored.failure_commit_seq,
                "catalog initial source-failure commit",
            )?
        && positive_u64(
            stored.failed_build_commit_seq,
            "catalog failed initial-source build commit",
        )? == expected.build_state_commit_seq
        && stored.coverage_plan_id.as_deref()
            == Some(expected.coverage_plan_id.storage_bytes().as_slice())
        && positive_u64(
            stored.readiness_epoch,
            "catalog initial source-failure epoch",
        )? == expected.epoch
        && positive_u64(stored.attempt, "catalog initial source-failure attempt")?
            == expected.attempt
        && stored.previous_state.as_deref() == Some(expected.state.as_str())
        && stored.reason_code.as_deref() == Some(reason_code)
        && stored.failed_at == committed_at
        && stored.failure_source.is_none()
        && stored.failure_reason.as_deref() == Some(INITIAL_SOURCE_UNAVAILABLE_REASON)
        && stored.failure_started_at == started_at
        && stored.failure_committed_at == Some(committed_at)
        && stored.failure_fact_count == 0
        && stored.build_source.is_none()
        && stored.build_reason.as_deref()
            == Some(if expected.retry_reason_code.is_some() {
                INITIAL_SOURCE_RETRYING_REASON
            } else {
                initial_build_execution_reason(expected.epoch, expected.attempt, expected.state)
            })
        && stored.build_committed_at.is_some()
        && stored.build_fact_count == 0)
}

#[allow(clippy::too_many_arguments)]
fn validate_initial_source_change(
    connection: &Connection,
    commit_seq: u64,
    plan: &CatalogCoveragePlan,
    desired_contract_version: u32,
    epoch: u64,
    attempt: u64,
    previous_state: CatalogDurableBuildPhase,
    state: CatalogReadinessPhase,
    transition: &'static str,
    source_count: usize,
    reason_code: Option<&str>,
) -> Result<u64, EngineError> {
    let predecessor: Option<i64> = connection
        .query_row(
            r#"
            SELECT MAX(commit_seq)
            FROM change_log
            WHERE topic = ?1
              AND entity_key = ?2
              AND commit_seq < ?3
            "#,
            params![
                READINESS_CHANGE_TOPIC,
                plan.coverage_plan_id.storage_bytes().as_slice(),
                to_i64(commit_seq, "catalog initial source change commit")?,
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("load initial catalog source predecessor", error))?;
    let predecessor = predecessor
        .map(|value| positive_u64(value, "catalog initial source predecessor"))
        .transpose()?
        .ok_or_else(|| {
            corrupt_catalog_state("catalog initial source change has no readiness predecessor")
        })?;
    let expected_payload = serde_json::to_vec(&CatalogInitialSourceChangedPayload {
        readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
        scope: LIBRARY_SCOPE,
        coverage_plan_id: plan.coverage_plan_id,
        desired_contract_version,
        epoch,
        attempt,
        previous_state: previous_state.readiness_phase(),
        state,
        transition,
        predecessor_state_commit: predecessor,
        source_count,
        reason_code,
        commit_seq,
    })
    .map_err(|_| corrupt_catalog_state("catalog initial source change cannot be encoded"))?;
    struct StoredInitialSourceChange {
        count: i64,
        topic: Option<String>,
        schema_version: i64,
        entity_key: Option<Vec<u8>>,
        operation: Option<String>,
        payload: Option<Vec<u8>>,
    }

    let row: Option<StoredInitialSourceChange> = connection
        .query_row(
            r#"
                SELECT COUNT(*) OVER (),
                       CASE WHEN typeof(topic) = 'text' THEN topic END,
                       schema_version,
                       CASE WHEN typeof(entity_key) = 'blob' AND length(entity_key) = 32
                            THEN entity_key END,
                       CASE WHEN typeof(operation) = 'text' THEN operation END,
                       CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND 65536
                            THEN payload END
                FROM change_log
                WHERE commit_seq = ?1
                ORDER BY ordinal
                LIMIT 1
                "#,
            [to_i64(commit_seq, "catalog initial source change commit")?],
            |row| {
                Ok(StoredInitialSourceChange {
                    count: row.get(0)?,
                    topic: row.get(1)?,
                    schema_version: row.get(2)?,
                    entity_key: row.get(3)?,
                    operation: row.get(4)?,
                    payload: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_error("load initial catalog source change", error))?;
    let Some(row) = row else {
        return Err(corrupt_catalog_state(
            "catalog initial source change is missing its outbox row",
        ));
    };
    if row.count != 1
        || row.topic.as_deref() != Some(READINESS_CHANGE_TOPIC)
        || row.schema_version != i64::from(INITIAL_SOURCE_CHANGE_SCHEMA_VERSION)
        || row.entity_key.as_deref() != Some(plan.coverage_plan_id.storage_bytes().as_slice())
        || row.operation.as_deref() != Some("upsert")
        || row.payload.as_deref() != Some(expected_payload.as_slice())
    {
        return Err(corrupt_catalog_state(
            "catalog initial source change does not bind its exact predecessor and transition",
        ));
    }
    Ok(predecessor)
}

fn exact_source_failure_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogActiveRefreshPublicationExpectation,
    reason_code: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let Some(stored) = load_latest_source_failure_evidence(connection)? else {
        return Ok(false);
    };
    let Some(retained) = expected
        .publication_identity
        .retained_chain()
        .last()
        .copied()
    else {
        return Ok(false);
    };
    Ok(current.readiness.state == CatalogReadinessPhase::Degraded
        && current.readiness.last_complete_snapshot == Some(expected.predecessor_snapshot)
        && current.readiness.complete_through_commit
            == (!expected.is_recovery()).then_some(expected.predecessor_snapshot.complete_commit)
        && current.readiness.reason
            == Some(CatalogReadinessReason::TerminalSourceUnavailable {
                code: reason_code.to_string(),
            })
        && current.last_commit_seq
            == positive_u64(stored.failure_commit_seq, "catalog source-failure commit")?
        && positive_u64(
            stored.failed_refresh_commit_seq,
            "catalog unavailable refresh commit",
        )? == expected.refresh_started_commit_seq
        && stored.coverage_plan_id.as_deref()
            == Some(expected.coverage_plan_id.storage_bytes().as_slice())
        && positive_u64(stored.readiness_epoch, "catalog source-failure epoch")? == expected.epoch
        && positive_u64(stored.attempt, "catalog source-failure attempt")? == expected.attempt
        && positive_u64(
            stored.retained_snapshot_commit_seq,
            "catalog source-failure retained snapshot",
        )? == expected.predecessor_snapshot.complete_commit
        && stored.retained_publication_digest.as_deref()
            == Some(retained.publication_digest().as_slice())
        && stored.retained_content_digest.as_deref() == Some(retained.content_digest().as_slice())
        && stored.reason_code.as_deref() == Some(reason_code)
        && stored.failed_at == committed_at
        && stored.failure_source.is_none()
        && stored.failure_reason.as_deref() == Some(REFRESH_SOURCE_UNAVAILABLE_REASON)
        && stored.failure_started_at == started_at
        && stored.failure_committed_at == Some(committed_at)
        && stored.failure_fact_count == 0
        && stored.refresh_source.is_none()
        && stored.refresh_reason.as_deref() == Some(refresh_execution_reason(expected))
        && stored.refresh_committed_at.is_some()
        && stored.refresh_fact_count == 0)
}

fn exact_source_retry_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogActiveRefreshPublicationExpectation,
    reason_code: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let actual = current.refresh_publication_expectation()?;
    let (source, reason, stored_started_at, stored_committed_at, fact_count): (
        Option<i64>,
        String,
        i64,
        Option<i64>,
        i64,
    ) = connection
        .query_row(
            r#"
            SELECT source_instance_id, reason, started_at, committed_at, fact_count
            FROM ingest_commits WHERE commit_seq = ?1
            "#,
            [to_i64(
                current.last_commit_seq,
                "catalog source-retry commit",
            )?],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|error| sqlite_error("load catalog source-retry commit", error))?;
    Ok(actual.scope == expected.scope
        && actual.coverage_plan_id == expected.coverage_plan_id
        && actual.desired_contract_version == expected.desired_contract_version
        && actual.epoch == expected.epoch
        && actual.attempt == expected.attempt
        && actual.predecessor_snapshot == expected.predecessor_snapshot
        && actual.lineage == expected.lineage
        && actual.publication_identity == expected.publication_identity
        && actual.retry_reason_code() == Some(reason_code)
        && actual.refresh_started_commit_seq > expected.refresh_started_commit_seq
        && source.is_none()
        && reason == REFRESH_SOURCE_RETRYING_REASON
        && stored_started_at == started_at
        && stored_committed_at == Some(committed_at)
        && fact_count == 0)
}

struct CatalogSourceFailureRestartContext<'a> {
    plan: &'a CatalogCoveragePlan,
    state_epoch: u64,
    state_attempt: u64,
    state_commit_seq: u64,
    updated_at: i64,
    retained_snapshot: CatalogSnapshotId,
    publication_identity: &'a CatalogReadyPublicationIdentity,
    publication_attempt: u64,
    recovering: bool,
}

fn validate_source_failure_for_restart(
    stored: &StoredCatalogSourceFailure,
    context: CatalogSourceFailureRestartContext<'_>,
) -> Result<(u64, String), EngineError> {
    let failure_commit = positive_u64(stored.failure_commit_seq, "catalog source-failure commit")?;
    let refresh_commit = positive_u64(
        stored.failed_refresh_commit_seq,
        "catalog unavailable refresh commit",
    )?;
    let failure_attempt = positive_u64(stored.attempt, "catalog source-failure attempt")?;
    let stored_snapshot = positive_u64(
        stored.retained_snapshot_commit_seq,
        "catalog source-failure retained snapshot",
    )?;
    let reason_code = stored.reason_code.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog source-failure reason is outside its machine-code bound")
    })?;
    validate_reason_code(reason_code).map_err(catalog_contract_error)?;
    let retained_commitment = context
        .publication_identity
        .retained_chain()
        .last()
        .copied()
        .ok_or_else(|| {
            corrupt_catalog_state("catalog source failure has no retained publication")
        })?;
    let state_matches = if context.recovering {
        context.state_attempt == failure_attempt.saturating_add(1)
            && context.state_commit_seq > failure_commit
    } else {
        context.state_attempt == failure_attempt && context.state_commit_seq == failure_commit
    };
    let epoch_replacement = context.state_epoch > context.retained_snapshot.readiness_epoch;
    let expected_refresh_reason = if epoch_replacement && failure_attempt == 1 {
        SOURCE_GENERATION_INVALIDATED_REASON
    } else if epoch_replacement || failure_attempt > context.publication_attempt {
        REFRESH_RECOVERY_STARTED_REASON
    } else {
        REFRESH_STARTED_REASON
    };
    if !state_matches
        || refresh_commit <= context.retained_snapshot.complete_commit
        || refresh_commit >= failure_commit
        || stored.coverage_plan_id.as_deref()
            != Some(context.plan.coverage_plan_id.storage_bytes().as_slice())
        || positive_u64(stored.readiness_epoch, "catalog source-failure epoch")?
            != context.state_epoch
        || stored_snapshot != context.retained_snapshot.complete_commit
        || retained_commitment.snapshot_id() != context.retained_snapshot
        || stored.retained_publication_digest.as_deref()
            != Some(retained_commitment.publication_digest().as_slice())
        || stored.retained_content_digest.as_deref()
            != Some(retained_commitment.content_digest().as_slice())
        || stored.failure_source.is_some()
        || stored.failure_reason.as_deref() != Some(REFRESH_SOURCE_UNAVAILABLE_REASON)
        || stored.failure_started_at > stored.failed_at
        || stored.failure_committed_at != Some(stored.failed_at)
        || stored.failure_fact_count != 0
        || stored.refresh_source.is_some()
        || !matches!(
            stored.refresh_reason.as_deref(),
            Some(reason)
                if reason == expected_refresh_reason
                    || reason == REFRESH_SOURCE_RETRYING_REASON
                    || reason == PARTIAL_REASON
        )
        || stored.refresh_committed_at.is_none()
        || stored.refresh_fact_count != 0
        || (!context.recovering && stored.failed_at != context.updated_at)
    {
        return Err(corrupt_catalog_state(
            "catalog source-failure evidence differs from its exact active refresh and retained publication",
        ));
    }
    Ok((failure_attempt, reason_code.to_string()))
}

struct CatalogIntegrityRestartContext<'a> {
    plan: &'a CatalogCoveragePlan,
    state_epoch: u64,
    state_attempt: u64,
    state_commit_seq: u64,
    updated_at: i64,
    retained_snapshot: CatalogSnapshotId,
    publication_identity: &'a CatalogReadyPublicationIdentity,
    publication_attempt: u64,
    recovering: bool,
}

fn validate_integrity_failure_for_restart(
    stored: &StoredCatalogIntegrityFailure,
    context: CatalogIntegrityRestartContext<'_>,
) -> Result<String, EngineError> {
    let failure_commit = positive_u64(
        stored.failure_commit_seq,
        "catalog integrity-failure commit",
    )?;
    let refresh_commit = positive_u64(
        stored.failed_refresh_commit_seq,
        "catalog failed refresh commit",
    )?;
    let failure_attempt = positive_u64(stored.attempt, "catalog integrity-failure attempt")?;
    let stored_snapshot = positive_u64(
        stored.retained_snapshot_commit_seq.ok_or_else(|| {
            corrupt_catalog_state(
                "independently-safe catalog integrity failure has no retained snapshot",
            )
        })?,
        "catalog failure retained snapshot",
    )?;
    let reason_code = stored.reason_code.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog integrity-failure reason is outside its machine-code bound")
    })?;
    validate_reason_code(reason_code).map_err(catalog_contract_error)?;
    let retained_commitment = context
        .publication_identity
        .retained_chain()
        .last()
        .copied()
        .ok_or_else(|| {
            corrupt_catalog_state("catalog integrity failure has no retained publication")
        })?;
    let state_matches = if context.recovering {
        context.state_attempt == failure_attempt.saturating_add(1)
            && context.state_commit_seq > failure_commit
    } else {
        context.state_attempt == failure_attempt && context.state_commit_seq == failure_commit
    };
    let epoch_replacement = context.state_epoch > context.retained_snapshot.readiness_epoch;
    let expected_refresh_reason = if epoch_replacement && failure_attempt == 1 {
        SOURCE_GENERATION_INVALIDATED_REASON
    } else if epoch_replacement || failure_attempt > context.publication_attempt {
        REFRESH_RECOVERY_STARTED_REASON
    } else {
        REFRESH_STARTED_REASON
    };
    if !state_matches
        || refresh_commit <= context.retained_snapshot.complete_commit
        || refresh_commit >= failure_commit
        || stored.coverage_plan_id.as_deref()
            != Some(context.plan.coverage_plan_id.storage_bytes().as_slice())
        || positive_u64(stored.readiness_epoch, "catalog failure epoch")? != context.state_epoch
        || stored_snapshot != context.retained_snapshot.complete_commit
        || retained_commitment.snapshot_id() != context.retained_snapshot
        || stored.retained_publication_digest.as_deref()
            != Some(retained_commitment.publication_digest().as_slice())
        || stored.retained_content_digest.as_deref()
            != Some(retained_commitment.content_digest().as_slice())
        || stored.snapshot_disposition.as_deref() != Some("independently_safe")
        || (!context.recovering && stored.failed_at != context.updated_at)
        || stored.failure_source.is_some()
        || stored.failure_reason.as_deref() != Some(REFRESH_INTEGRITY_FAILURE_REASON)
        || stored.failure_started_at > stored.failed_at
        || stored.failure_committed_at != Some(stored.failed_at)
        || stored.failure_fact_count != 0
        || stored.refresh_source.is_some()
        || !matches!(
            stored.refresh_reason.as_deref(),
            Some(reason)
                if reason == expected_refresh_reason
                    || reason == REFRESH_SOURCE_RETRYING_REASON
                    || reason == PARTIAL_REASON
        )
        || stored.refresh_committed_at.is_none()
        || stored.refresh_fact_count != 0
    {
        return Err(corrupt_catalog_state(
            "catalog integrity-failure evidence differs from its exact active refresh and retained publication",
        ));
    }
    Ok(reason_code.to_string())
}

struct CatalogDiscardedIntegrityRestartContext<'a> {
    plan: &'a CatalogCoveragePlan,
    state_epoch: u64,
    state_attempt: u64,
    state_commit_seq: u64,
    updated_at: i64,
    recovering: bool,
}

fn validate_discarded_integrity_failure_for_restart(
    stored: &StoredCatalogIntegrityFailure,
    context: CatalogDiscardedIntegrityRestartContext<'_>,
) -> Result<String, EngineError> {
    let failure_commit = positive_u64(
        stored.failure_commit_seq,
        "catalog initial integrity-failure commit",
    )?;
    let execution_commit = positive_u64(
        stored.failed_refresh_commit_seq,
        "catalog failed initial-build commit",
    )?;
    let failure_attempt = positive_u64(stored.attempt, "catalog initial failure attempt")?;
    let reason_code = stored.reason_code.as_deref().ok_or_else(|| {
        corrupt_catalog_state(
            "catalog initial integrity-failure reason is outside its machine-code bound",
        )
    })?;
    validate_reason_code(reason_code).map_err(catalog_contract_error)?;
    let state_matches = if context.recovering {
        context.state_attempt == failure_attempt.saturating_add(1)
            && context.state_commit_seq > failure_commit
    } else {
        context.state_attempt == failure_attempt && context.state_commit_seq == failure_commit
    };
    let base_execution_reason = initial_build_execution_reason(
        context.state_epoch,
        failure_attempt,
        CatalogDurableBuildPhase::Building,
    );
    if !state_matches
        || execution_commit >= failure_commit
        || stored.coverage_plan_id.as_deref()
            != Some(context.plan.coverage_plan_id.storage_bytes().as_slice())
        || positive_u64(stored.readiness_epoch, "catalog initial failure epoch")?
            != context.state_epoch
        || stored.retained_snapshot_commit_seq.is_some()
        || stored.retained_publication_digest.is_some()
        || stored.retained_content_digest.is_some()
        || stored.snapshot_disposition.as_deref() != Some("discarded")
        || (!context.recovering && stored.failed_at != context.updated_at)
        || stored.failure_source.is_some()
        || stored.failure_reason.as_deref() != Some(INITIAL_INTEGRITY_FAILURE_REASON)
        || stored.failure_started_at > stored.failed_at
        || stored.failure_committed_at != Some(stored.failed_at)
        || stored.failure_fact_count != 0
        || stored.refresh_source.is_some()
        || !matches!(
            stored.refresh_reason.as_deref(),
            Some(reason)
                if reason == base_execution_reason
                    || reason == PARTIAL_REASON
                    || reason == INITIAL_SOURCE_RETRYING_REASON
        )
        || stored.refresh_committed_at.is_none()
        || stored.refresh_fact_count != 0
    {
        return Err(corrupt_catalog_state(
            "discarded catalog integrity-failure evidence differs from its exact no-snapshot build lineage",
        ));
    }
    Ok(reason_code.to_string())
}

struct CatalogInitialSourceFailureRestartContext<'a> {
    plan: &'a CatalogCoveragePlan,
    state_epoch: u64,
    state_attempt: u64,
    state_commit_seq: u64,
    updated_at: i64,
    recovering: bool,
}

fn validate_initial_source_failure_for_restart(
    stored: &StoredCatalogInitialSourceFailure,
    context: CatalogInitialSourceFailureRestartContext<'_>,
) -> Result<(CatalogDurableBuildPhase, String), EngineError> {
    let failure_commit = positive_u64(
        stored.failure_commit_seq,
        "catalog initial source-failure commit",
    )?;
    let failed_build_commit = positive_u64(
        stored.failed_build_commit_seq,
        "catalog failed initial-source build commit",
    )?;
    let evidence_attempt = positive_u64(stored.attempt, "catalog initial source-failure attempt")?;
    let expected_attempt = if context.recovering {
        context.state_attempt.checked_sub(1).ok_or_else(|| {
            corrupt_catalog_state("catalog initial source recovery attempt underflow")
        })?
    } else {
        context.state_attempt
    };
    let previous_state =
        CatalogDurableBuildPhase::parse(stored.previous_state.as_deref().ok_or_else(|| {
            corrupt_catalog_state("catalog initial source failure has an invalid prior state")
        })?)?;
    let reason_code = stored.reason_code.as_deref().ok_or_else(|| {
        corrupt_catalog_state("catalog initial source failure reason is malformed")
    })?;
    validate_reason_code(reason_code).map_err(|_| {
        corrupt_catalog_state("catalog initial source failure reason is outside its bound")
    })?;
    let base_execution_reason =
        initial_build_execution_reason(context.state_epoch, expected_attempt, previous_state);
    if stored.coverage_plan_id.as_deref()
        != Some(context.plan.coverage_plan_id.storage_bytes().as_slice())
        || positive_u64(
            stored.readiness_epoch,
            "catalog initial source-failure epoch",
        )? != context.state_epoch
        || evidence_attempt != expected_attempt
        || !matches!(
            previous_state,
            CatalogDurableBuildPhase::Building | CatalogDurableBuildPhase::Partial
        )
        || failed_build_commit >= failure_commit
        || (context.recovering && failure_commit >= context.state_commit_seq)
        || (!context.recovering && failure_commit != context.state_commit_seq)
        || (!context.recovering && stored.failed_at != context.updated_at)
        || stored.failure_source.is_some()
        || stored.failure_reason.as_deref() != Some(INITIAL_SOURCE_UNAVAILABLE_REASON)
        || stored.failure_started_at > stored.failed_at
        || stored.failure_committed_at != Some(stored.failed_at)
        || stored.failure_fact_count != 0
        || stored.build_source.is_some()
        || !matches!(
            stored.build_reason.as_deref(),
            Some(reason)
                if reason == base_execution_reason || reason == INITIAL_SOURCE_RETRYING_REASON
        )
        || stored.build_committed_at.is_none()
        || stored.build_fact_count != 0
    {
        return Err(corrupt_catalog_state(
            "initial catalog source-failure evidence differs from its exact no-snapshot build lineage",
        ));
    }
    Ok((previous_state, reason_code.to_string()))
}

struct CatalogColdRecoveryRestartContext<'a> {
    initial_source_failure: Option<&'a StoredCatalogInitialSourceFailure>,
    integrity_failure: Option<&'a StoredCatalogIntegrityFailure>,
    plan: &'a CatalogCoveragePlan,
    epoch: u64,
    attempt: u64,
    state_commit_seq: u64,
    updated_at: i64,
}

fn validate_cold_recovery_origin(
    connection: &Connection,
    context: CatalogColdRecoveryRestartContext<'_>,
) -> Result<CatalogDurableBuildPhase, EngineError> {
    let prior_attempt = context.attempt.checked_sub(1).ok_or_else(|| {
        corrupt_catalog_state("cold catalog recovery attempt cannot be decremented")
    })?;
    let source_matches = context
        .initial_source_failure
        .map(|failure| {
            Ok(positive_u64(
                failure.readiness_epoch,
                "catalog initial source-failure epoch",
            )? == context.epoch
                && positive_u64(failure.attempt, "catalog initial source-failure attempt")?
                    == prior_attempt)
        })
        .transpose()?
        .unwrap_or(false);
    let integrity_matches = context
        .integrity_failure
        .map(|failure| {
            Ok(positive_u64(
                failure.readiness_epoch,
                "catalog discarded integrity-failure epoch",
            )? == context.epoch
                && positive_u64(
                    failure.attempt,
                    "catalog discarded integrity-failure attempt",
                )? == prior_attempt
                && failure.snapshot_disposition.as_deref() == Some("discarded"))
        })
        .transpose()?
        .unwrap_or(false);
    match (
        source_matches,
        integrity_matches,
        context.initial_source_failure,
        context.integrity_failure,
    ) {
        (true, false, Some(failure), _) => {
            let (previous_state, reason_code) = validate_initial_source_failure_for_restart(
                failure,
                CatalogInitialSourceFailureRestartContext {
                    plan: context.plan,
                    state_epoch: context.epoch,
                    state_attempt: context.attempt,
                    state_commit_seq: context.state_commit_seq,
                    updated_at: context.updated_at,
                    recovering: true,
                },
            )?;
            let failure_commit = positive_u64(
                failure.failure_commit_seq,
                "catalog initial source-failure commit",
            )?;
            let prior_coverage = if previous_state == CatalogDurableBuildPhase::Partial {
                load_and_validate_partial_history(
                    connection,
                    context.plan,
                    Some(CatalogPartialCoverageTarget::LatestBefore {
                        epoch: context.epoch,
                        attempt: prior_attempt,
                        commit: failure_commit,
                    }),
                )?
                .ok_or_else(|| {
                    corrupt_catalog_state(
                        "cold source recovery is missing its prior Partial milestone",
                    )
                })?
            } else {
                Vec::new()
            };
            let unavailable = unavailable_initial_source_coverage(
                context.plan,
                CATALOG_QUERY_PACK_CONTRACT_VERSION,
                &prior_coverage,
            )?;
            let predecessor = validate_initial_source_change(
                connection,
                failure_commit,
                context.plan,
                CATALOG_QUERY_PACK_CONTRACT_VERSION,
                context.epoch,
                prior_attempt,
                previous_state,
                CatalogReadinessPhase::Degraded,
                "source_unavailable",
                unavailable.len(),
                Some(&reason_code),
            )?;
            if predecessor
                != positive_u64(
                    failure.failed_build_commit_seq,
                    "catalog failed initial-source build commit",
                )?
            {
                return Err(corrupt_catalog_state(
                    "cold source recovery failure change has a foreign predecessor",
                ));
            }
            Ok(CatalogDurableBuildPhase::Degraded)
        }
        (false, true, _, Some(failure)) => {
            validate_discarded_integrity_failure_for_restart(
                failure,
                CatalogDiscardedIntegrityRestartContext {
                    plan: context.plan,
                    state_epoch: context.epoch,
                    state_attempt: context.attempt,
                    state_commit_seq: context.state_commit_seq,
                    updated_at: context.updated_at,
                    recovering: true,
                },
            )?;
            Ok(CatalogDurableBuildPhase::Error)
        }
        _ => Err(corrupt_catalog_state(
            "cold catalog recovery does not identify exactly one prior terminal failure",
        )),
    }
}

fn write_build_state(
    transaction: &Transaction<'_>,
    machine: &mut CatalogReadinessMachine,
    commit_seq: u64,
    updated_at: i64,
    write: CatalogBuildStateWrite<'_>,
) -> Result<(), EngineError> {
    let snapshot = machine.snapshot();
    let phase = durable_phase(snapshot.state)?;
    let changed = match write {
        CatalogBuildStateWrite::InsertPlan => transaction
            .execute(
                r#"
                INSERT INTO catalog_build_state (
                    scope_kind, coverage_plan_id, desired_contract_version,
                    epoch, attempt, state, last_commit_seq, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    LIBRARY_SCOPE,
                    snapshot.coverage_plan_id.storage_bytes().as_slice(),
                    i64::from(snapshot.desired_contract_version),
                    to_i64(snapshot.epoch, "catalog readiness epoch")?,
                    to_i64(snapshot.attempt, "catalog readiness attempt")?,
                    phase.as_str(),
                    to_i64(commit_seq, "catalog state commit")?,
                    updated_at,
                ],
            )
            .map_err(|error| sqlite_error("insert catalog build state", error))?,
        CatalogBuildStateWrite::Schedule { expected } => transaction
            .execute(
                r#"
                UPDATE catalog_build_state
                SET state = ?1, last_commit_seq = ?2, updated_at = ?3
                WHERE scope_kind = ?4
                  AND coverage_plan_id = ?5
                  AND desired_contract_version = ?6
                  AND epoch = ?7
                  AND attempt = ?8
                  AND state = ?9
                  AND last_commit_seq = ?10
                "#,
                params![
                    phase.as_str(),
                    to_i64(commit_seq, "catalog state commit")?,
                    updated_at,
                    LIBRARY_SCOPE,
                    snapshot.coverage_plan_id.storage_bytes().as_slice(),
                    i64::from(snapshot.desired_contract_version),
                    to_i64(snapshot.epoch, "catalog readiness epoch")?,
                    to_i64(snapshot.attempt, "catalog readiness attempt")?,
                    PENDING_STATE,
                    to_i64(expected.state_commit_seq, "catalog pending state commit")?,
                ],
            )
            .map_err(|error| sqlite_error("schedule catalog build state", error))?,
        CatalogBuildStateWrite::RecordPartial { expected, .. } => {
            if phase != CatalogDurableBuildPhase::Partial
                || snapshot.complete_through_commit.is_some()
                || snapshot.refreshing_from_snapshot.is_some()
                || snapshot.reason.is_some()
            {
                return Err(EngineError::InvalidCommit(
                    "catalog partial write differs from its active build lineage".to_string(),
                ));
            }
            let retained_snapshot_commit = snapshot
                .last_complete_snapshot
                .map(|value| to_i64(value.complete_commit, "catalog retained partial snapshot"))
                .transpose()?;
            transaction
                .execute(
                    r#"
                    UPDATE catalog_build_state
                    SET state = 'partial', last_commit_seq = ?1, updated_at = ?2
                    WHERE scope_kind = ?3
                      AND coverage_plan_id = ?4
                      AND desired_contract_version = ?5
                      AND epoch = ?6
                      AND attempt = ?7
                      AND state = ?8
                      AND completed_contract_version IS ?9
                      AND complete_through_commit IS NULL
                      AND last_complete_snapshot_commit IS ?10
                      AND refreshing_from_snapshot_commit IS NULL
                      AND reason_code IS NULL
                      AND last_commit_seq = ?11
                    "#,
                    params![
                        to_i64(commit_seq, "catalog partial state commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(snapshot.epoch, "catalog partial epoch")?,
                        to_i64(snapshot.attempt, "catalog partial attempt")?,
                        expected.state.as_str(),
                        snapshot.completed_contract_version.map(i64::from),
                        retained_snapshot_commit,
                        to_i64(
                            expected.state_commit_seq,
                            "catalog partial predecessor commit"
                        )?,
                    ],
                )
                .map_err(|error| sqlite_error("record catalog partial progress", error))?
        }
        CatalogBuildStateWrite::InvalidateSourceGeneration { expected } => {
            if phase != CatalogDurableBuildPhase::Building
                || snapshot.epoch != expected.epoch.saturating_add(1)
                || snapshot.attempt != 1
                || snapshot.complete_through_commit.is_some()
                || snapshot.refreshing_from_snapshot.is_some()
                || snapshot.reason.is_some()
                || !snapshot.source_coverage.is_empty()
                || snapshot.last_complete_snapshot != expected.last_complete_snapshot
            {
                return Err(EngineError::InvalidCommit(
                    "catalog source-generation invalidation differs from its exact successor epoch"
                        .to_string(),
                ));
            }
            transaction
                .execute(
                    r#"
                    UPDATE catalog_build_state
                    SET epoch = ?1,
                        attempt = 1,
                        state = 'building',
                        complete_through_commit = NULL,
                        refreshing_from_snapshot_commit = NULL,
                        reason_code = NULL,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = ?9
                      AND last_complete_snapshot_commit IS ?10
                      AND last_commit_seq = ?11
                    "#,
                    params![
                        to_i64(snapshot.epoch, "catalog readiness epoch")?,
                        to_i64(commit_seq, "catalog epoch invalidation commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(expected.epoch, "catalog previous readiness epoch")?,
                        to_i64(expected.attempt, "catalog previous readiness attempt")?,
                        expected.state.as_str(),
                        expected
                            .last_complete_snapshot
                            .map(|value| {
                                to_i64(
                                    value.complete_commit,
                                    "catalog invalidation retained snapshot",
                                )
                            })
                            .transpose()?,
                        to_i64(
                            expected.state_commit_seq,
                            "catalog epoch invalidation predecessor",
                        )?,
                    ],
                )
                .map_err(|error| sqlite_error("invalidate catalog source generation", error))?
        }
        CatalogBuildStateWrite::BeginRefresh { expected } => {
            let refreshing = snapshot.refreshing_from_snapshot.ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog refresh write is missing its retained snapshot".to_string(),
                )
            })?;
            if phase != CatalogDurableBuildPhase::Ready
                || refreshing != expected.snapshot_id
                || snapshot.last_complete_snapshot != Some(expected.snapshot_id)
                || snapshot.complete_through_commit != Some(expected.snapshot_id.complete_commit)
            {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh write differs from its exact Ready expectation".to_string(),
                ));
            }
            transaction
                .execute(
                    r#"
                    UPDATE catalog_build_state
                    SET refreshing_from_snapshot_commit = ?1,
                        reason_code = NULL,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = 'ready'
                      AND completed_contract_version = ?6
                      AND complete_through_commit = ?1
                      AND last_complete_snapshot_commit = ?1
                      AND refreshing_from_snapshot_commit IS NULL
                      AND last_commit_seq = ?9
                    "#,
                    params![
                        to_i64(
                            refreshing.complete_commit,
                            "catalog refreshing snapshot commit"
                        )?,
                        to_i64(commit_seq, "catalog refresh state commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(snapshot.epoch, "catalog readiness epoch")?,
                        to_i64(snapshot.attempt, "catalog readiness attempt")?,
                        to_i64(expected.state_commit_seq, "catalog expected Ready commit")?,
                    ],
                )
                .map_err(|error| sqlite_error("begin catalog ordinary refresh", error))?
        }
        CatalogBuildStateWrite::FailInitialBuildIntegrity {
            expected,
            reason_code,
        } => {
            let Some(CatalogReadinessReason::IntegrityFailure {
                code,
                snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
            }) = snapshot.reason.as_ref()
            else {
                return Err(EngineError::InvalidCommit(
                    "catalog initial integrity-failure write is missing discarded evidence"
                        .to_string(),
                ));
            };
            if phase != CatalogDurableBuildPhase::Error
                || code != reason_code
                || snapshot.completed_contract_version.is_some()
                || snapshot.complete_through_commit.is_some()
                || snapshot.last_complete_snapshot.is_some()
                || snapshot.refreshing_from_snapshot.is_some()
            {
                return Err(EngineError::InvalidCommit(
                    "catalog initial integrity-failure write differs from its exact Building lineage"
                        .to_string(),
                ));
            }
            transaction
                .execute(
                    r#"
                    UPDATE catalog_build_state
                    SET state = 'error',
                        reason_code = ?1,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = ?9
                      AND completed_contract_version IS NULL
                      AND complete_through_commit IS NULL
                      AND last_complete_snapshot_commit IS NULL
                      AND refreshing_from_snapshot_commit IS NULL
                      AND reason_code IS ?10
                      AND last_commit_seq = ?11
                    "#,
                    params![
                        reason_code,
                        to_i64(commit_seq, "catalog initial integrity-failure commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(snapshot.epoch, "catalog readiness epoch")?,
                        to_i64(snapshot.attempt, "catalog readiness attempt")?,
                        expected.state.as_str(),
                        expected.retry_reason_code.as_deref(),
                        to_i64(
                            expected.build_started_commit_seq,
                            "catalog failed initial-build commit",
                        )?,
                    ],
                )
                .map_err(|error| sqlite_error("fail initial catalog build integrity", error))?
        }
        CatalogBuildStateWrite::MarkInitialBuildSourceRetrying {
            expected,
            reason_code,
        } => {
            let Some(CatalogReadinessReason::SourceRetrying { code }) = snapshot.reason.as_ref()
            else {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source-retry write is missing retry evidence".to_string(),
                ));
            };
            if code != reason_code
                || phase != expected.state
                || snapshot.completed_contract_version.is_some()
                || snapshot.complete_through_commit.is_some()
                || snapshot.last_complete_snapshot.is_some()
                || snapshot.refreshing_from_snapshot.is_some()
            {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source-retry write differs from its exact active build"
                        .to_string(),
                ));
            }
            transaction
                .execute(
                    r#"
                    UPDATE catalog_build_state
                    SET reason_code = ?1,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = ?9
                      AND completed_contract_version IS NULL
                      AND complete_through_commit IS NULL
                      AND last_complete_snapshot_commit IS NULL
                      AND refreshing_from_snapshot_commit IS NULL
                      AND reason_code IS ?10
                      AND last_commit_seq = ?11
                    "#,
                    params![
                        reason_code,
                        to_i64(commit_seq, "catalog initial source-retry commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(snapshot.epoch, "catalog readiness epoch")?,
                        to_i64(snapshot.attempt, "catalog readiness attempt")?,
                        expected.state.as_str(),
                        expected.retry_reason_code.as_deref(),
                        to_i64(
                            expected.build_state_commit_seq,
                            "catalog initial retry predecessor commit",
                        )?,
                    ],
                )
                .map_err(|error| sqlite_error("mark initial catalog source retry", error))?
        }
        CatalogBuildStateWrite::DegradeInitialBuildSource {
            expected,
            reason_code,
        } => {
            let Some(CatalogReadinessReason::TerminalSourceUnavailable { code }) =
                snapshot.reason.as_ref()
            else {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source-failure write is missing terminal evidence".to_string(),
                ));
            };
            if code != reason_code
                || phase != CatalogDurableBuildPhase::Degraded
                || snapshot.completed_contract_version.is_some()
                || snapshot.complete_through_commit.is_some()
                || snapshot.last_complete_snapshot.is_some()
                || snapshot.refreshing_from_snapshot.is_some()
            {
                return Err(EngineError::InvalidCommit(
                    "catalog initial source-failure write differs from its exact active build"
                        .to_string(),
                ));
            }
            transaction
                .execute(
                    r#"
                    UPDATE catalog_build_state
                    SET state = 'degraded',
                        reason_code = ?1,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = ?9
                      AND completed_contract_version IS NULL
                      AND complete_through_commit IS NULL
                      AND last_complete_snapshot_commit IS NULL
                      AND refreshing_from_snapshot_commit IS NULL
                      AND reason_code IS ?10
                      AND last_commit_seq = ?11
                    "#,
                    params![
                        reason_code,
                        to_i64(commit_seq, "catalog initial source-failure commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(snapshot.epoch, "catalog readiness epoch")?,
                        to_i64(snapshot.attempt, "catalog readiness attempt")?,
                        expected.state.as_str(),
                        expected.retry_reason_code.as_deref(),
                        to_i64(
                            expected.build_state_commit_seq,
                            "catalog failed initial-source build commit",
                        )?,
                    ],
                )
                .map_err(|error| sqlite_error("degrade initial catalog source", error))?
        }
        CatalogBuildStateWrite::FailActiveRefreshIntegrity {
            expected,
            reason_code,
        } => {
            let Some(CatalogReadinessReason::IntegrityFailure {
                code,
                snapshot_disposition: CatalogIntegritySnapshotDisposition::IndependentlySafe,
            }) = snapshot.reason.as_ref()
            else {
                return Err(EngineError::InvalidCommit(
                    "catalog integrity-failure write is missing independently-safe evidence"
                        .to_string(),
                ));
            };
            if phase != CatalogDurableBuildPhase::Error
                || code != reason_code
                || snapshot.refreshing_from_snapshot.is_some()
                || snapshot.last_complete_snapshot != Some(expected.predecessor_snapshot)
                || snapshot.complete_through_commit
                    != (!expected.is_epoch_replacement())
                        .then_some(expected.predecessor_snapshot.complete_commit)
            {
                return Err(EngineError::InvalidCommit(
                    "catalog integrity-failure write differs from its exact active refresh"
                        .to_string(),
                ));
            }
            if expected.is_recovery() {
                let retained_snapshot_commit = to_i64(
                    expected.predecessor_snapshot.complete_commit,
                    "catalog recovery retained snapshot commit",
                )?;
                let complete_through_commit =
                    (!expected.is_epoch_replacement()).then_some(retained_snapshot_commit);
                transaction
                    .execute(
                        r#"
                        UPDATE catalog_build_state
                        SET state = 'error',
                            complete_through_commit = ?1,
                            reason_code = ?2,
                            last_commit_seq = ?3,
                            updated_at = ?4
                        WHERE scope_kind = ?5
                          AND coverage_plan_id = ?6
                          AND desired_contract_version = ?7
                          AND epoch = ?8
                          AND attempt = ?9
                          AND state = ?12
                          AND completed_contract_version = ?7
                          AND complete_through_commit IS NULL
                          AND last_complete_snapshot_commit = ?13
                          AND refreshing_from_snapshot_commit IS NULL
                          AND reason_code IS ?10
                          AND last_commit_seq = ?11
                        "#,
                        params![
                            complete_through_commit,
                            reason_code,
                            to_i64(commit_seq, "catalog recovery integrity-failure commit")?,
                            updated_at,
                            LIBRARY_SCOPE,
                            snapshot.coverage_plan_id.storage_bytes().as_slice(),
                            i64::from(snapshot.desired_contract_version),
                            to_i64(snapshot.epoch, "catalog readiness epoch")?,
                            to_i64(snapshot.attempt, "catalog readiness attempt")?,
                            expected.retry_reason_code(),
                            to_i64(
                                expected.refresh_started_commit_seq,
                                "catalog failed recovery commit",
                            )?,
                            expected.durable_state().as_str(),
                            retained_snapshot_commit,
                        ],
                    )
                    .map_err(|error| {
                        sqlite_error("fail recovering catalog refresh integrity", error)
                    })?
            } else {
                transaction
                    .execute(
                        r#"
                    UPDATE catalog_build_state
                    SET state = 'error',
                        refreshing_from_snapshot_commit = NULL,
                        reason_code = ?1,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = 'ready'
                      AND completed_contract_version = ?6
                      AND complete_through_commit = ?9
                      AND last_complete_snapshot_commit = ?9
                      AND refreshing_from_snapshot_commit = ?9
                      AND reason_code IS ?10
                      AND last_commit_seq = ?11
                    "#,
                        params![
                            reason_code,
                            to_i64(commit_seq, "catalog integrity-failure commit")?,
                            updated_at,
                            LIBRARY_SCOPE,
                            snapshot.coverage_plan_id.storage_bytes().as_slice(),
                            i64::from(snapshot.desired_contract_version),
                            to_i64(snapshot.epoch, "catalog readiness epoch")?,
                            to_i64(snapshot.attempt, "catalog readiness attempt")?,
                            to_i64(
                                expected.predecessor_snapshot.complete_commit,
                                "catalog retained snapshot commit",
                            )?,
                            expected.retry_reason_code(),
                            to_i64(
                                expected.refresh_started_commit_seq,
                                "catalog failed refresh commit",
                            )?,
                        ],
                    )
                    .map_err(|error| sqlite_error("fail active catalog refresh integrity", error))?
            }
        }
        CatalogBuildStateWrite::MarkActiveRefreshRetrying {
            expected,
            reason_code,
        } => {
            let Some(CatalogReadinessReason::SourceRetrying { code }) = snapshot.reason.as_ref()
            else {
                return Err(EngineError::InvalidCommit(
                    "catalog source-retry write is missing retry evidence".to_string(),
                ));
            };
            if code != reason_code
                || snapshot.last_complete_snapshot != Some(expected.predecessor_snapshot)
                || snapshot.refreshing_from_snapshot
                    != (!expected.is_recovery()).then_some(expected.predecessor_snapshot)
                || snapshot.complete_through_commit
                    != (!expected.is_recovery())
                        .then_some(expected.predecessor_snapshot.complete_commit)
            {
                return Err(EngineError::InvalidCommit(
                    "catalog source-retry write differs from its exact refresh".to_string(),
                ));
            }
            let expected_state = expected.durable_state().as_str();
            transaction
                .execute(
                    r#"
                    UPDATE catalog_build_state
                    SET reason_code = ?1,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = ?9
                      AND completed_contract_version = ?6
                      AND last_complete_snapshot_commit = ?10
                      AND reason_code IS NULL
                      AND last_commit_seq = ?11
                    "#,
                    params![
                        reason_code,
                        to_i64(commit_seq, "catalog source-retry commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(snapshot.epoch, "catalog readiness epoch")?,
                        to_i64(snapshot.attempt, "catalog readiness attempt")?,
                        expected_state,
                        to_i64(
                            expected.predecessor_snapshot.complete_commit,
                            "catalog retry retained snapshot commit",
                        )?,
                        to_i64(
                            expected.refresh_started_commit_seq,
                            "catalog retry predecessor state commit",
                        )?,
                    ],
                )
                .map_err(|error| sqlite_error("mark catalog source retry", error))?
        }
        CatalogBuildStateWrite::DegradeActiveRefresh {
            expected,
            reason_code,
        } => {
            let Some(CatalogReadinessReason::TerminalSourceUnavailable { code }) =
                snapshot.reason.as_ref()
            else {
                return Err(EngineError::InvalidCommit(
                    "catalog source-failure write is missing terminal evidence".to_string(),
                ));
            };
            if phase != CatalogDurableBuildPhase::Degraded
                || code != reason_code
                || snapshot.refreshing_from_snapshot.is_some()
                || snapshot.last_complete_snapshot != Some(expected.predecessor_snapshot)
                || snapshot.complete_through_commit
                    != (!expected.is_recovery())
                        .then_some(expected.predecessor_snapshot.complete_commit)
            {
                return Err(EngineError::InvalidCommit(
                    "catalog source-failure write differs from its exact active refresh"
                        .to_string(),
                ));
            }
            if expected.is_recovery() {
                transaction
                    .execute(
                        r#"
                        UPDATE catalog_build_state
                        SET state = 'degraded',
                            reason_code = ?1,
                            last_commit_seq = ?2,
                            updated_at = ?3
                        WHERE scope_kind = ?4
                          AND coverage_plan_id = ?5
                          AND desired_contract_version = ?6
                          AND epoch = ?7
                          AND attempt = ?8
                          AND state = ?12
                          AND completed_contract_version = ?6
                          AND complete_through_commit IS NULL
                          AND last_complete_snapshot_commit = ?9
                          AND refreshing_from_snapshot_commit IS NULL
                          AND reason_code IS ?10
                          AND last_commit_seq = ?11
                        "#,
                        params![
                            reason_code,
                            to_i64(commit_seq, "catalog recovery source-failure commit")?,
                            updated_at,
                            LIBRARY_SCOPE,
                            snapshot.coverage_plan_id.storage_bytes().as_slice(),
                            i64::from(snapshot.desired_contract_version),
                            to_i64(snapshot.epoch, "catalog readiness epoch")?,
                            to_i64(snapshot.attempt, "catalog readiness attempt")?,
                            to_i64(
                                expected.predecessor_snapshot.complete_commit,
                                "catalog recovery retained snapshot commit",
                            )?,
                            expected.retry_reason_code(),
                            to_i64(
                                expected.refresh_started_commit_seq,
                                "catalog recovery attempt commit",
                            )?,
                            expected.durable_state().as_str(),
                        ],
                    )
                    .map_err(|error| sqlite_error("degrade recovering catalog refresh", error))?
            } else {
                transaction
                    .execute(
                        r#"
                    UPDATE catalog_build_state
                    SET state = 'degraded',
                        refreshing_from_snapshot_commit = NULL,
                        reason_code = ?1,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = 'ready'
                      AND completed_contract_version = ?6
                      AND complete_through_commit = ?9
                      AND last_complete_snapshot_commit = ?9
                      AND refreshing_from_snapshot_commit = ?9
                      AND reason_code IS ?10
                      AND last_commit_seq = ?11
                    "#,
                        params![
                            reason_code,
                            to_i64(commit_seq, "catalog source-failure commit")?,
                            updated_at,
                            LIBRARY_SCOPE,
                            snapshot.coverage_plan_id.storage_bytes().as_slice(),
                            i64::from(snapshot.desired_contract_version),
                            to_i64(snapshot.epoch, "catalog readiness epoch")?,
                            to_i64(snapshot.attempt, "catalog readiness attempt")?,
                            to_i64(
                                expected.predecessor_snapshot.complete_commit,
                                "catalog retained snapshot commit",
                            )?,
                            expected.retry_reason_code(),
                            to_i64(
                                expected.refresh_started_commit_seq,
                                "catalog unavailable refresh commit",
                            )?,
                        ],
                    )
                    .map_err(|error| sqlite_error("degrade active catalog refresh", error))?
            }
        }
        CatalogBuildStateWrite::RetryTerminalRefresh { expected } => {
            if phase != CatalogDurableBuildPhase::Building
                || snapshot.reason.is_some()
                || snapshot.refreshing_from_snapshot.is_some()
                || snapshot.complete_through_commit.is_some()
            {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery write differs from terminal readiness".to_string(),
                ));
            }
            let prior_attempt = snapshot.attempt.checked_sub(1).ok_or_else(|| {
                EngineError::InvalidCommit("catalog recovery attempt underflow".to_string())
            })?;
            if snapshot.last_complete_snapshot.is_none() {
                if !matches!(
                    expected.state,
                    CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Error
                ) || snapshot.completed_contract_version.is_some()
                    || !snapshot.source_coverage.is_empty()
                {
                    return Err(EngineError::InvalidCommit(
                        "cold catalog recovery differs from its no-snapshot terminal state"
                            .to_string(),
                    ));
                }
                transaction
                    .execute(
                        r#"
                        UPDATE catalog_build_state
                        SET state = 'building',
                            attempt = ?1,
                            reason_code = NULL,
                            last_commit_seq = ?2,
                            updated_at = ?3
                        WHERE scope_kind = ?4
                          AND coverage_plan_id = ?5
                          AND desired_contract_version = ?6
                          AND epoch = ?7
                          AND attempt = ?8
                          AND state = ?9
                          AND completed_contract_version IS NULL
                          AND complete_through_commit IS NULL
                          AND last_complete_snapshot_commit IS NULL
                          AND refreshing_from_snapshot_commit IS NULL
                          AND reason_code IS NOT NULL
                          AND last_commit_seq = ?10
                        "#,
                        params![
                            to_i64(snapshot.attempt, "catalog recovery attempt")?,
                            to_i64(commit_seq, "catalog recovery commit")?,
                            updated_at,
                            LIBRARY_SCOPE,
                            snapshot.coverage_plan_id.storage_bytes().as_slice(),
                            i64::from(snapshot.desired_contract_version),
                            to_i64(snapshot.epoch, "catalog readiness epoch")?,
                            to_i64(prior_attempt, "catalog prior terminal attempt")?,
                            expected.state.as_str(),
                            to_i64(
                                expected.state_commit_seq,
                                "catalog prior terminal state commit",
                            )?,
                        ],
                    )
                    .map_err(|error| sqlite_error("start cold catalog recovery", error))?
            } else {
                transaction
                    .execute(
                        r#"
                    UPDATE catalog_build_state
                    SET state = 'building',
                        attempt = ?1,
                        complete_through_commit = NULL,
                        reason_code = NULL,
                        last_commit_seq = ?2,
                        updated_at = ?3
                    WHERE scope_kind = ?4
                      AND coverage_plan_id = ?5
                      AND desired_contract_version = ?6
                      AND epoch = ?7
                      AND attempt = ?8
                      AND state = ?9
                      AND completed_contract_version = ?6
                      AND (
                        complete_through_commit IS NULL
                        OR complete_through_commit = last_complete_snapshot_commit
                      )
                      AND last_complete_snapshot_commit IS NOT NULL
                      AND refreshing_from_snapshot_commit IS NULL
                      AND reason_code IS NOT NULL
                      AND last_commit_seq = ?10
                    "#,
                        params![
                            to_i64(snapshot.attempt, "catalog recovery attempt")?,
                            to_i64(commit_seq, "catalog recovery commit")?,
                            updated_at,
                            LIBRARY_SCOPE,
                            snapshot.coverage_plan_id.storage_bytes().as_slice(),
                            i64::from(snapshot.desired_contract_version),
                            to_i64(snapshot.epoch, "catalog readiness epoch")?,
                            to_i64(prior_attempt, "catalog prior terminal attempt")?,
                            expected.state.as_str(),
                            to_i64(
                                expected.state_commit_seq,
                                "catalog prior terminal state commit",
                            )?,
                        ],
                    )
                    .map_err(|error| sqlite_error("start terminal catalog recovery", error))?
            }
        }
    };
    if changed != 1 {
        return Err(EngineError::InvalidCommit(
            "catalog build-state compare-and-swap changed no row".to_string(),
        ));
    }
    Ok(())
}

fn write_readiness_change(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    snapshot: &CatalogReadinessSnapshot,
    write: CatalogBuildStateWrite<'_>,
) -> Result<(), EngineError> {
    let initial_source_change = match write {
        CatalogBuildStateWrite::MarkInitialBuildSourceRetrying {
            expected,
            reason_code,
        } => Some((expected, "source_retrying", Some(reason_code))),
        CatalogBuildStateWrite::DegradeInitialBuildSource {
            expected,
            reason_code,
        } => Some((expected, "source_unavailable", Some(reason_code))),
        _ => None,
    };
    let cold_recovery_expected = match write {
        CatalogBuildStateWrite::RetryTerminalRefresh { expected }
            if snapshot.last_complete_snapshot.is_none()
                && expected.state == CatalogDurableBuildPhase::Degraded =>
        {
            Some(expected)
        }
        _ => None,
    };
    let (schema_version, payload) = if let Some((expected, transition, reason_code)) =
        initial_source_change
    {
        let payload = serde_json::to_vec(&CatalogInitialSourceChangedPayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            previous_state: expected.state.readiness_phase(),
            state: snapshot.state,
            transition,
            predecessor_state_commit: expected.build_state_commit_seq,
            source_count: snapshot.source_coverage.len(),
            reason_code,
            commit_seq,
        });
        (INITIAL_SOURCE_CHANGE_SCHEMA_VERSION, payload)
    } else if let Some(expected) = cold_recovery_expected {
        let payload = serde_json::to_vec(&CatalogInitialSourceChangedPayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            previous_state: CatalogReadinessPhase::Degraded,
            state: snapshot.state,
            transition: "recovery_started",
            predecessor_state_commit: expected.state_commit_seq,
            source_count: snapshot.source_coverage.len(),
            reason_code: None,
            commit_seq,
        });
        (INITIAL_SOURCE_CHANGE_SCHEMA_VERSION, payload)
    } else if let CatalogBuildStateWrite::InvalidateSourceGeneration { expected } = write {
        if snapshot.state != CatalogReadinessPhase::Building
            || snapshot.epoch != expected.epoch.saturating_add(1)
            || snapshot.attempt != 1
            || snapshot.complete_through_commit.is_some()
            || snapshot.refreshing_from_snapshot.is_some()
            || snapshot.reason.is_some()
            || !snapshot.source_coverage.is_empty()
            || snapshot.last_complete_snapshot != expected.last_complete_snapshot
        {
            return Err(EngineError::InvalidCommit(
                "catalog epoch invalidation does not identify its exact successor lineage"
                    .to_string(),
            ));
        }
        let payload = serde_json::to_vec(&CatalogEpochInvalidatedPayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            previous_epoch: expected.epoch,
            previous_attempt: expected.attempt,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            previous_state: expected.state.readiness_phase(),
            state: snapshot.state,
            predecessor_state_commit: expected.state_commit_seq,
            last_complete_snapshot: snapshot.last_complete_snapshot,
            commit_seq,
        });
        (EPOCH_INVALIDATION_CHANGE_SCHEMA_VERSION, payload)
    } else if snapshot.state == CatalogReadinessPhase::Partial {
        if snapshot.source_coverage.is_empty()
            || snapshot.complete_through_commit.is_some()
            || snapshot.refreshing_from_snapshot.is_some()
            || !matches!(
                snapshot.reason.as_ref(),
                None | Some(CatalogReadinessReason::SourceRetrying { .. })
            )
        {
            return Err(EngineError::InvalidCommit(
                "catalog Partial invalidation does not identify one bounded progress milestone"
                    .to_string(),
            ));
        }
        let reason_code = match snapshot.reason.as_ref() {
            None => None,
            Some(CatalogReadinessReason::SourceRetrying { code }) => {
                validate_reason_code(code).map_err(catalog_contract_error)?;
                Some(code.clone())
            }
            Some(_) => unreachable!("Partial reason shape checked above"),
        };
        let payload = serde_json::to_vec(&CatalogPartialChangedPayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            state: snapshot.state,
            source_count: snapshot.source_coverage.len(),
            reason_code,
            commit_seq,
        });
        (PARTIAL_CHANGE_SCHEMA_VERSION, payload)
    } else if snapshot.state == CatalogReadinessPhase::Degraded {
        let completed_contract_version = snapshot.completed_contract_version.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog source-unavailable invalidation is missing its completed contract"
                    .to_string(),
            )
        })?;
        let last_complete_snapshot = snapshot.last_complete_snapshot.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog source-unavailable invalidation is missing its retained snapshot"
                    .to_string(),
            )
        })?;
        let complete_through_commit = snapshot.complete_through_commit;
        let Some(CatalogReadinessReason::TerminalSourceUnavailable { code }) =
            snapshot.reason.as_ref()
        else {
            return Err(EngineError::InvalidCommit(
                "catalog source-unavailable invalidation is missing its typed reason".to_string(),
            ));
        };
        validate_reason_code(code).map_err(catalog_contract_error)?;
        if snapshot.refreshing_from_snapshot.is_some()
            || complete_through_commit
                .is_some_and(|commit| commit != last_complete_snapshot.complete_commit)
        {
            return Err(EngineError::InvalidCommit(
                "catalog source-unavailable invalidation does not retain one exact snapshot"
                    .to_string(),
            ));
        }
        let payload = serde_json::to_vec(&CatalogRefreshSourceUnavailablePayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            completed_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            state: snapshot.state,
            last_complete_snapshot,
            complete_through_commit,
            reason_code: code,
            commit_seq,
        });
        (SOURCE_UNAVAILABLE_CHANGE_SCHEMA_VERSION, payload)
    } else if snapshot.state == CatalogReadinessPhase::Building
        && snapshot.last_complete_snapshot.is_some()
    {
        let completed_contract_version = snapshot.completed_contract_version.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog recovery invalidation is missing its retained contract".to_string(),
            )
        })?;
        let last_complete_snapshot = snapshot.last_complete_snapshot.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog recovery invalidation is missing its retained snapshot".to_string(),
            )
        })?;
        let retry_reason = match snapshot.reason.as_ref() {
            None => None,
            Some(CatalogReadinessReason::SourceRetrying { code }) => {
                validate_reason_code(code).map_err(catalog_contract_error)?;
                Some(code.clone())
            }
            Some(_) => {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery invalidation has a terminal reason".to_string(),
                ));
            }
        };
        if snapshot.complete_through_commit.is_some() || snapshot.refreshing_from_snapshot.is_some()
        {
            return Err(EngineError::InvalidCommit(
                "catalog recovery invalidation is not one exact Building retry".to_string(),
            ));
        }
        let payload = serde_json::to_vec(&CatalogRefreshRecoveryStartedPayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            completed_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            state: snapshot.state,
            last_complete_snapshot,
            reason_code: retry_reason,
            commit_seq,
        });
        (SOURCE_UNAVAILABLE_CHANGE_SCHEMA_VERSION, payload)
    } else if snapshot.state == CatalogReadinessPhase::Error
        && snapshot.last_complete_snapshot.is_none()
    {
        let Some(CatalogReadinessReason::IntegrityFailure {
            code,
            snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
        }) = snapshot.reason.as_ref()
        else {
            return Err(EngineError::InvalidCommit(
                "catalog initial integrity failure is missing its discarded typed reason"
                    .to_string(),
            ));
        };
        validate_reason_code(code).map_err(catalog_contract_error)?;
        if snapshot.completed_contract_version.is_some()
            || snapshot.complete_through_commit.is_some()
            || snapshot.refreshing_from_snapshot.is_some()
            || !snapshot.source_coverage.is_empty()
        {
            return Err(EngineError::InvalidCommit(
                "catalog initial integrity failure retains forbidden snapshot state".to_string(),
            ));
        }
        let payload = serde_json::to_vec(&CatalogInitialIntegrityFailurePayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            state: snapshot.state,
            reason_code: code,
            snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
            commit_seq,
        });
        (INITIAL_INTEGRITY_FAILURE_CHANGE_SCHEMA_VERSION, payload)
    } else if snapshot.state == CatalogReadinessPhase::Error {
        let completed_contract_version = snapshot.completed_contract_version.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog integrity-failure invalidation is missing its completed contract"
                    .to_string(),
            )
        })?;
        let last_complete_snapshot = snapshot.last_complete_snapshot.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog integrity-failure invalidation is missing its retained snapshot"
                    .to_string(),
            )
        })?;
        let complete_through_commit = snapshot.complete_through_commit;
        let Some(CatalogReadinessReason::IntegrityFailure {
            code,
            snapshot_disposition: CatalogIntegritySnapshotDisposition::IndependentlySafe,
        }) = snapshot.reason.as_ref()
        else {
            return Err(EngineError::InvalidCommit(
                "catalog integrity-failure invalidation is missing its safe typed reason"
                    .to_string(),
            ));
        };
        validate_reason_code(code).map_err(catalog_contract_error)?;
        if snapshot.refreshing_from_snapshot.is_some()
            || complete_through_commit
                .is_some_and(|commit| commit != last_complete_snapshot.complete_commit)
        {
            return Err(EngineError::InvalidCommit(
                "catalog integrity-failure invalidation does not retain one exact snapshot"
                    .to_string(),
            ));
        }
        let payload = serde_json::to_vec(&CatalogRefreshIntegrityFailurePayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            completed_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            state: snapshot.state,
            last_complete_snapshot,
            complete_through_commit,
            reason_code: code,
            snapshot_disposition: CatalogIntegritySnapshotDisposition::IndependentlySafe,
            commit_seq,
        });
        (INTEGRITY_FAILURE_CHANGE_SCHEMA_VERSION, payload)
    } else if let Some(refreshing) = snapshot.refreshing_from_snapshot {
        let completed_contract_version = snapshot.completed_contract_version.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog refresh invalidation is missing its completed contract".to_string(),
            )
        })?;
        let last_complete_snapshot = snapshot.last_complete_snapshot.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog refresh invalidation is missing its retained snapshot".to_string(),
            )
        })?;
        let complete_through_commit = snapshot.complete_through_commit.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog refresh invalidation is missing its complete commit".to_string(),
            )
        })?;
        if refreshing != last_complete_snapshot
            || complete_through_commit != refreshing.complete_commit
        {
            return Err(EngineError::InvalidCommit(
                "catalog refresh invalidation does not identify one exact retained snapshot"
                    .to_string(),
            ));
        }
        let retry_reason = match snapshot.reason.as_ref() {
            None => None,
            Some(CatalogReadinessReason::SourceRetrying { code }) => {
                validate_reason_code(code).map_err(catalog_contract_error)?;
                Some(code.clone())
            }
            Some(_) => {
                return Err(EngineError::InvalidCommit(
                    "catalog refresh invalidation has a terminal reason".to_string(),
                ));
            }
        };
        let payload = serde_json::to_vec(&CatalogRefreshStartedPayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            completed_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            state: snapshot.state,
            last_complete_snapshot,
            refreshing_from_snapshot: refreshing,
            complete_through_commit,
            reason_code: retry_reason.clone(),
            commit_seq,
        });
        (
            if retry_reason.is_some() {
                SOURCE_UNAVAILABLE_CHANGE_SCHEMA_VERSION
            } else {
                REFRESH_CHANGE_SCHEMA_VERSION
            },
            payload,
        )
    } else {
        let payload = serde_json::to_vec(&CatalogReadinessChangedPayload {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: LIBRARY_SCOPE,
            coverage_plan_id: snapshot.coverage_plan_id,
            desired_contract_version: snapshot.desired_contract_version,
            epoch: snapshot.epoch,
            attempt: snapshot.attempt,
            state: snapshot.state,
            commit_seq,
        });
        (READINESS_CHANGE_SCHEMA_VERSION, payload)
    };
    let payload = payload.map_err(|error| {
        EngineError::InvalidCommit(format!(
            "could not encode catalog readiness change: {error}"
        ))
    })?;
    commit::write_internal_changes(
        transaction,
        commit_seq,
        &[ChangeEntry {
            topic: READINESS_CHANGE_TOPIC.to_string(),
            schema_version,
            entity_key: snapshot.coverage_plan_id.storage_bytes().to_vec(),
            operation: "upsert".to_string(),
            payload,
        }],
    )
}

pub(super) fn validate_admin_commit(
    source_instance_id: Option<i64>,
    reason: &str,
    committed_at: Option<i64>,
    fact_count: i64,
    expected_reason: &str,
) -> Result<(), EngineError> {
    if source_instance_id.is_some()
        || reason != expected_reason
        || committed_at.is_none()
        || fact_count != 0
    {
        return Err(corrupt_catalog_state(
            "catalog build lineage is not owned by the expected source-neutral commit",
        ));
    }
    Ok(())
}

fn phase_reason(
    phase: CatalogDurableBuildPhase,
    refreshing: bool,
) -> Result<&'static str, EngineError> {
    match (phase, refreshing) {
        (CatalogDurableBuildPhase::Pending, false) => Ok(REGISTER_REASON),
        (CatalogDurableBuildPhase::Building, false) => Ok(SCHEDULE_REASON),
        (CatalogDurableBuildPhase::Partial, false) => Ok(PARTIAL_REASON),
        (CatalogDurableBuildPhase::Ready, false) => Ok(INITIAL_PUBLICATION_REASON),
        (CatalogDurableBuildPhase::Ready, true) => Ok(REFRESH_STARTED_REASON),
        (CatalogDurableBuildPhase::Degraded, false) => Ok(REFRESH_SOURCE_UNAVAILABLE_REASON),
        (CatalogDurableBuildPhase::Error, false) => Ok(REFRESH_INTEGRITY_FAILURE_REASON),
        (
            CatalogDurableBuildPhase::Pending
            | CatalogDurableBuildPhase::Building
            | CatalogDurableBuildPhase::Partial
            | CatalogDurableBuildPhase::Degraded
            | CatalogDurableBuildPhase::Error,
            true,
        ) => Err(corrupt_catalog_state(
            "pending/building/error catalog state cannot own a refresh commit",
        )),
    }
}

fn durable_phase(state: CatalogReadinessPhase) -> Result<CatalogDurableBuildPhase, EngineError> {
    match state {
        CatalogReadinessPhase::Pending => Ok(CatalogDurableBuildPhase::Pending),
        CatalogReadinessPhase::Building => Ok(CatalogDurableBuildPhase::Building),
        CatalogReadinessPhase::Partial => Ok(CatalogDurableBuildPhase::Partial),
        CatalogReadinessPhase::Ready => Ok(CatalogDurableBuildPhase::Ready),
        CatalogReadinessPhase::Degraded => Ok(CatalogDurableBuildPhase::Degraded),
        CatalogReadinessPhase::Error => Ok(CatalogDurableBuildPhase::Error),
    }
}

pub(super) fn positive_u32(value: i64, field: &'static str) -> Result<u32, EngineError> {
    let value = u32::try_from(value)
        .map_err(|_| corrupt_catalog_state(format!("{field} is outside the durable u32 range")))?;
    if value == 0 {
        return Err(corrupt_catalog_state(format!("{field} must be positive")));
    }
    Ok(value)
}

pub(super) fn positive_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    let value =
        u64::try_from(value).map_err(|_| corrupt_catalog_state(format!("{field} is negative")))?;
    if value == 0 {
        return Err(corrupt_catalog_state(format!("{field} must be positive")));
    }
    Ok(value)
}

pub(super) fn to_i64(value: u64, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} exceeds SQLite integer range")))
}

pub(super) fn catalog_contract_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::InvalidCommit(format!("invalid catalog contract: {error}"))
}

pub(super) fn corrupt_catalog_state(message: impl Into<String>) -> EngineError {
    EngineError::InvalidCommit(format!(
        "corrupt durable catalog build state: {}",
        message.into()
    ))
}

pub(super) fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{
        CanonicalEntityKey, CanonicalSourceInstanceKey, CoverageDeclarationDigest, CoverageDomain,
        CoverageMembershipRevision, ExternalEntityRef,
    };
    use crate::catalog_contract::{
        CatalogAccessPolicyDigest, CatalogCoveragePlanSource, CATALOG_PROJECTION_PACK_ID,
    };
    use crate::core::schema;
    use crate::engine::{writer::WriterRuntime, EngineOptions, SpaghettiEngineCore};
    use tempfile::tempdir;

    const PRECOMMIT_STAGES: [CatalogCommitStage; 6] = [
        CatalogCommitStage::BeforeTransaction,
        CatalogCommitStage::AfterCommitInsert,
        CatalogCommitStage::AfterPlanWrite,
        CatalogCommitStage::AfterBuildStateWrite,
        CatalogCommitStage::AfterOutboxInsert,
        CatalogCommitStage::BeforeCommit,
    ];

    struct FailAt(CatalogCommitStage);

    impl CatalogCommitHook for FailAt {
        fn reach(&self, stage: CatalogCommitStage) -> Result<(), EngineError> {
            if stage == self.0 {
                Err(EngineError::InjectedFailure {
                    stage: stage_name(stage),
                })
            } else {
                Ok(())
            }
        }
    }

    fn stage_name(stage: CatalogCommitStage) -> &'static str {
        match stage {
            CatalogCommitStage::BeforeTransaction => "before catalog transaction",
            CatalogCommitStage::AfterCommitInsert => "after catalog commit insert",
            CatalogCommitStage::AfterPlanWrite => "after catalog plan write",
            CatalogCommitStage::AfterPartialCoverageWrite => "after catalog partial-coverage write",
            CatalogCommitStage::AfterEpochInvalidationWrite => {
                "after catalog epoch-invalidation write"
            }
            CatalogCommitStage::AfterFailureEvidenceWrite => {
                "after catalog integrity-failure evidence write"
            }
            CatalogCommitStage::AfterBuildStateWrite => "after catalog build-state write",
            CatalogCommitStage::AfterOutboxInsert => "after catalog outbox insert",
            CatalogCommitStage::BeforeCommit => "before catalog commit",
            CatalogCommitStage::AfterCommit => "after catalog commit",
        }
    }

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
    }

    fn plan_source(label: &str, policy: &[u8]) -> CatalogCoveragePlanSource {
        CatalogCoveragePlanSource::new(
            label,
            CanonicalSourceInstanceKey::derive(1, format!("fixture/{label}").as_bytes()).unwrap(),
            format!("{label}@candidate-v1"),
            CoverageDeclarationDigest::derive(format!("{label}-catalog-v1").as_bytes()).unwrap(),
            CatalogAccessPolicyDigest::derive(1, policy).unwrap(),
        )
        .unwrap()
    }

    fn plan() -> CatalogCoveragePlan {
        CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![
                plan_source("claude-code", b"local-library-view"),
                plan_source("codex", b"local-library-view"),
            ],
            vec![plan_source("grok", b"local-library-view")],
        )
        .unwrap()
    }

    fn other_plan() -> CatalogCoveragePlan {
        CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![
                plan_source("claude-code", b"local-library-view"),
                plan_source("codex", b"restricted-library-view"),
            ],
            vec![plan_source("grok", b"local-library-view")],
        )
        .unwrap()
    }

    fn register(plan: CatalogCoveragePlan) -> CatalogBuildStateCommand {
        CatalogBuildStateCommand::register(plan, 1, 10, 11)
    }

    fn coverage(
        plan: &CatalogCoveragePlan,
        adapter_id: &str,
        completeness: CoverageSetCompleteness,
    ) -> SourceCoverageSet {
        let source = plan
            .required_sources
            .iter()
            .chain(plan.optional_sources.iter())
            .find(|source| source.adapter_id == adapter_id)
            .unwrap();
        SourceCoverageSet::new(
            CoverageDomain::ProjectionPack {
                pack: CATALOG_PROJECTION_PACK_ID.to_string(),
                version: 1,
            },
            source.coverage_scope(CatalogCoverageScope::Library),
            CoverageMembershipRevision::derive(format!("{adapter_id}-members-v1").as_bytes())
                .unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            completeness,
        )
        .unwrap()
    }

    fn scheduled_database() -> (Connection, CatalogCoveragePlan) {
        let mut connection = database();
        let plan = plan();
        apply_catalog_build_state_commit(&mut connection, &register(plan.clone()))
            .unwrap()
            .unwrap();
        let pending = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        (connection, plan)
    }

    #[test]
    fn cold_source_failure_is_restart_safe_and_retries_without_query_authority() {
        let (mut connection, plan) = scheduled_database();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        let retrying_command = CatalogBuildStateCommand::mark_initial_build_source_retrying(
            building.initial_source_expectation().unwrap(),
            "catalog_source_temporarily_unavailable",
            30,
            31,
        );
        let retrying = apply_catalog_build_state_commit(&mut connection, &retrying_command)
            .unwrap()
            .unwrap();
        assert_eq!(retrying.readiness.state, CatalogReadinessPhase::Building);
        assert_eq!(
            retrying.readiness.reason,
            Some(CatalogReadinessReason::SourceRetrying {
                code: "catalog_source_temporarily_unavailable".to_string(),
            })
        );
        assert!(retrying.readiness.source_coverage.is_empty());
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &retrying_command).unwrap(),
            None
        );
        let restarted_retry = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(restarted_retry.readiness, retrying.readiness);
        assert!(restarted_retry.ready_read_authority().is_err());

        let terminal_command = CatalogBuildStateCommand::degrade_initial_build_source(
            restarted_retry.initial_source_expectation().unwrap(),
            "catalog_source_retry_exhausted",
            40,
            41,
        );
        let degraded = apply_catalog_build_state_commit(&mut connection, &terminal_command)
            .unwrap()
            .unwrap();
        assert_eq!(degraded.readiness.state, CatalogReadinessPhase::Degraded);
        assert_eq!(degraded.readiness.last_complete_snapshot, None);
        assert_eq!(degraded.readiness.completed_contract_version, None);
        assert_eq!(degraded.readiness.source_coverage.len(), 2);
        assert!(degraded
            .readiness
            .source_coverage
            .iter()
            .all(|coverage| coverage.completeness == CoverageSetCompleteness::Unavailable));
        assert!(plan.required_coverage_present(&degraded.readiness.source_coverage));
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &terminal_command).unwrap(),
            None
        );
        let restarted_degraded = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(restarted_degraded.readiness, degraded.readiness);
        assert!(restarted_degraded.ready_read_authority().is_err());
        assert_eq!(count(&connection, "catalog_initial_source_failures"), 1);

        let recovery_command = CatalogBuildStateCommand::retry_terminal_refresh(
            restarted_degraded.expectation().unwrap(),
            50,
            51,
        );
        let recovered = apply_catalog_build_state_commit(&mut connection, &recovery_command)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.readiness.state, CatalogReadinessPhase::Building);
        assert_eq!(recovered.readiness.attempt, 2);
        assert!(recovered.readiness.source_coverage.is_empty());
        assert_eq!(
            load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .recovery_origin,
            Some(CatalogDurableBuildPhase::Degraded)
        );
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &recovery_command).unwrap(),
            None
        );
        let schema_versions: Vec<i64> = connection
            .prepare("SELECT schema_version FROM change_log WHERE commit_seq IN (3, 4, 5) ORDER BY commit_seq")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            schema_versions,
            vec![
                i64::from(INITIAL_SOURCE_CHANGE_SCHEMA_VERSION),
                i64::from(INITIAL_SOURCE_CHANGE_SCHEMA_VERSION),
                i64::from(INITIAL_SOURCE_CHANGE_SCHEMA_VERSION),
            ]
        );
    }

    #[test]
    fn cold_partial_source_failure_preserves_only_unavailable_progress() {
        let (mut connection, plan) = scheduled_database();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                building.partial_expectation().unwrap(),
                vec![coverage(
                    &plan,
                    "claude-code",
                    CoverageSetCompleteness::Complete,
                )],
                30,
                31,
            ),
        )
        .unwrap()
        .unwrap();
        let partial = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::mark_initial_build_source_retrying(
                partial.initial_source_expectation().unwrap(),
                "catalog_source_temporarily_unavailable",
                40,
                41,
            ),
        )
        .unwrap()
        .unwrap();
        let retrying = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(retrying.readiness.state, CatalogReadinessPhase::Partial);
        assert_eq!(retrying.readiness.source_coverage.len(), 1);
        assert_eq!(
            retrying.readiness.source_coverage[0].completeness,
            CoverageSetCompleteness::Partial
        );
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::degrade_initial_build_source(
                retrying.initial_source_expectation().unwrap(),
                "catalog_source_retry_exhausted",
                50,
                51,
            ),
        )
        .unwrap()
        .unwrap();
        let degraded = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(degraded.readiness.state, CatalogReadinessPhase::Degraded);
        assert_eq!(degraded.readiness.source_coverage.len(), 2);
        assert!(degraded
            .readiness
            .source_coverage
            .iter()
            .all(|coverage| coverage.completeness == CoverageSetCompleteness::Unavailable));
    }

    #[test]
    fn cold_source_failure_is_atomic_and_rejects_corrupt_evidence() {
        let precommit_stages = [
            CatalogCommitStage::BeforeTransaction,
            CatalogCommitStage::AfterCommitInsert,
            CatalogCommitStage::AfterPlanWrite,
            CatalogCommitStage::AfterFailureEvidenceWrite,
            CatalogCommitStage::AfterBuildStateWrite,
            CatalogCommitStage::AfterOutboxInsert,
            CatalogCommitStage::BeforeCommit,
        ];
        for stage in precommit_stages {
            let (mut connection, _) = scheduled_database();
            let building = load_catalog_build_state(&connection).unwrap().unwrap();
            let command = CatalogBuildStateCommand::degrade_initial_build_source(
                building.initial_source_expectation().unwrap(),
                "catalog_source_retry_exhausted",
                30,
                31,
            );
            let result = apply_catalog_build_state_commit_with_hook(
                &mut connection,
                &command,
                &FailAt(stage),
            );
            assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
            let state = load_catalog_build_state(&connection).unwrap().unwrap();
            assert_eq!(state.readiness.state, CatalogReadinessPhase::Building);
            assert_eq!(count(&connection, "catalog_initial_source_failures"), 0);
        }

        let (mut connection, _) = scheduled_database();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        let command = CatalogBuildStateCommand::degrade_initial_build_source(
            building.initial_source_expectation().unwrap(),
            "catalog_source_retry_exhausted",
            30,
            31,
        );
        assert!(matches!(
            apply_catalog_build_state_commit_with_hook(
                &mut connection,
                &command,
                &FailAt(CatalogCommitStage::AfterCommit),
            ),
            Err(EngineError::InjectedFailure { .. })
        ));
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &command).unwrap(),
            None
        );

        let corrupt = |statement: &str| {
            let (mut connection, _) = scheduled_database();
            let building = load_catalog_build_state(&connection).unwrap().unwrap();
            apply_catalog_build_state_commit(
                &mut connection,
                &CatalogBuildStateCommand::degrade_initial_build_source(
                    building.initial_source_expectation().unwrap(),
                    "catalog_source_retry_exhausted",
                    30,
                    31,
                ),
            )
            .unwrap()
            .unwrap();
            connection
                .execute_batch(
                    "DROP TRIGGER catalog_initial_source_failures_no_update; \
                     PRAGMA foreign_keys = OFF; \
                     PRAGMA ignore_check_constraints = ON;",
                )
                .unwrap();
            connection.execute_batch(statement).unwrap();
            load_catalog_build_state(&connection).unwrap_err()
        };
        assert!(
            corrupt("UPDATE catalog_initial_source_failures SET failed_build_commit_seq = 1;")
                .to_string()
                .contains("source-failure ledger")
        );
        assert!(
            corrupt("UPDATE change_log SET payload = x'7b7d' WHERE commit_seq = 3;")
                .to_string()
                .contains("source change")
        );
    }

    #[test]
    fn cold_source_and_integrity_failures_select_only_the_prior_attempt() {
        let (mut connection, _) = scheduled_database();
        let first = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::degrade_initial_build_source(
                first.initial_source_expectation().unwrap(),
                "first_source_failure",
                30,
                31,
            ),
        )
        .unwrap()
        .unwrap();
        let terminal = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::retry_terminal_refresh(
                terminal.expectation().unwrap(),
                40,
                41,
            ),
        )
        .unwrap()
        .unwrap();
        let second = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::fail_initial_build_integrity(
                second.initial_integrity_expectation().unwrap(),
                "second_integrity_failure",
                50,
                51,
            ),
        )
        .unwrap()
        .unwrap();
        let terminal = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::retry_terminal_refresh(
                terminal.expectation().unwrap(),
                60,
                61,
            ),
        )
        .unwrap()
        .unwrap();
        let third = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(third.recovery_origin, Some(CatalogDurableBuildPhase::Error));
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::degrade_initial_build_source(
                third.initial_source_expectation().unwrap(),
                "third_source_failure",
                70,
                71,
            ),
        )
        .unwrap()
        .unwrap();
        let terminal = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::retry_terminal_refresh(
                terminal.expectation().unwrap(),
                80,
                81,
            ),
        )
        .unwrap()
        .unwrap();
        let fourth = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(
            fourth.recovery_origin,
            Some(CatalogDurableBuildPhase::Degraded)
        );
        assert_eq!(fourth.readiness.attempt, 4);
    }

    fn counts(connection: &Connection) -> (i64, i64, i64, i64) {
        (
            count(connection, "ingest_commits"),
            count(connection, "catalog_coverage_plans"),
            count(connection, "catalog_build_state"),
            count(connection, "change_log"),
        )
    }

    fn count(connection: &Connection, table: &str) -> i64 {
        let query = match table {
            "ingest_commits" => "SELECT COUNT(*) FROM ingest_commits",
            "catalog_coverage_plans" => "SELECT COUNT(*) FROM catalog_coverage_plans",
            "catalog_build_state" => "SELECT COUNT(*) FROM catalog_build_state",
            "catalog_initial_source_failures" => {
                "SELECT COUNT(*) FROM catalog_initial_source_failures"
            }
            "change_log" => "SELECT COUNT(*) FROM change_log",
            _ => unreachable!(),
        };
        connection.query_row(query, [], |row| row.get(0)).unwrap()
    }

    fn assert_initial_shape(snapshot: &CatalogReadinessSnapshot, state: CatalogReadinessPhase) {
        assert_eq!(snapshot.state, state);
        assert_eq!(snapshot.scope, CatalogCoverageScope::Library);
        assert_eq!(snapshot.desired_contract_version, 1);
        assert_eq!(snapshot.epoch, 1);
        assert_eq!(snapshot.attempt, 1);
        assert_eq!(snapshot.completed_contract_version, None);
        assert_eq!(snapshot.complete_through_commit, None);
        assert_eq!(snapshot.last_complete_snapshot, None);
        assert_eq!(snapshot.refreshing_from_snapshot, None);
        assert!(snapshot.source_coverage.is_empty());
        assert_eq!(snapshot.reason, None);
    }

    #[test]
    fn registers_and_schedules_only_the_initial_library_lineage() {
        let mut connection = database();
        let registered = apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        assert_eq!(registered.commit_seq, 1);
        assert_initial_shape(&registered.readiness, CatalogReadinessPhase::Pending);
        assert_eq!(counts(&connection), (1, 1, 1, 1));

        let source_instance_id: Option<i64> = connection
            .query_row(
                "SELECT source_instance_id FROM ingest_commits WHERE commit_seq = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_instance_id, None);

        let (topic, schema_version, entity_key, operation, payload): (
            String,
            i64,
            Vec<u8>,
            String,
            Vec<u8>,
        ) = connection
            .query_row(
                r#"
                SELECT topic, schema_version, entity_key, operation, payload
                FROM change_log WHERE commit_seq = 1 AND ordinal = 0
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(topic, READINESS_CHANGE_TOPIC);
        assert_eq!(schema_version, i64::from(READINESS_CHANGE_SCHEMA_VERSION));
        assert_eq!(operation, "upsert");
        assert_eq!(
            entity_key,
            registered.readiness.coverage_plan_id.storage_bytes()
        );
        let payload_value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload_value["scope"], LIBRARY_SCOPE);
        assert_eq!(payload_value["state"], PENDING_STATE);
        assert_eq!(payload_value["commit_seq"], 1);
        let payload_text = String::from_utf8(payload).unwrap();
        assert!(!payload_text.contains("claude-code"));
        assert!(!payload_text.contains("fixture/"));

        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &register(plan())).unwrap(),
            None
        );
        assert_eq!(counts(&connection), (1, 1, 1, 1));

        let pending = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_initial_shape(&pending.readiness, CatalogReadinessPhase::Pending);
        let expectation = pending.expectation().unwrap();
        let schedule = CatalogBuildStateCommand::schedule(expectation.clone(), 20, 21);
        let scheduled = apply_catalog_build_state_commit(&mut connection, &schedule)
            .unwrap()
            .unwrap();
        assert_eq!(scheduled.commit_seq, 2);
        assert_initial_shape(&scheduled.readiness, CatalogReadinessPhase::Building);
        assert_eq!(counts(&connection), (2, 1, 1, 2));
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &schedule).unwrap(),
            None
        );
        assert_eq!(counts(&connection), (2, 1, 1, 2));

        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(building.last_commit_seq, 2);
        assert_initial_shape(&building.readiness, CatalogReadinessPhase::Building);
        assert!(connection
            .execute(
                "UPDATE catalog_build_state SET state = 'ready' WHERE scope_kind = 'library'",
                [],
            )
            .is_err());
    }

    #[test]
    fn registration_and_scheduling_fail_closed_on_every_cas_axis() {
        let mut connection = database();
        apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        let pending = load_catalog_build_state(&connection).unwrap().unwrap();
        let expected = pending.expectation().unwrap();

        for conflicting_registration in [
            register(other_plan()),
            CatalogBuildStateCommand::register(plan(), 2, 10, 11),
        ] {
            assert!(
                apply_catalog_build_state_commit(&mut connection, &conflicting_registration)
                    .is_err()
            );
            assert_eq!(counts(&connection), (1, 1, 1, 1));
        }

        let source_key = CanonicalSourceInstanceKey::derive(1, b"foreign-scope").unwrap();
        let foreign_entity =
            CanonicalEntityKey::derive("fixture", &source_key, "catalog.session", b"foreign")
                .unwrap();
        let mut wrong_scope = expected.clone();
        wrong_scope.scope = CatalogCoverageScope::Entity {
            external_ref: ExternalEntityRef::new(foreign_entity),
        };
        let mut wrong_plan = expected.clone();
        wrong_plan.coverage_plan_id = other_plan().coverage_plan_id;
        let mut wrong_version = expected.clone();
        wrong_version.desired_contract_version += 1;
        let mut wrong_epoch = expected.clone();
        wrong_epoch.epoch += 1;
        let mut wrong_attempt = expected.clone();
        wrong_attempt.attempt += 1;
        let mut wrong_state = expected.clone();
        wrong_state.state = CatalogDurableBuildPhase::Building;

        for forged in [
            wrong_scope,
            wrong_plan,
            wrong_version,
            wrong_epoch,
            wrong_attempt,
            wrong_state,
        ] {
            let command = CatalogBuildStateCommand::schedule(forged, 20, 21);
            assert!(apply_catalog_build_state_commit(&mut connection, &command).is_err());
            assert_eq!(counts(&connection), (1, 1, 1, 1));
            let retained = load_catalog_build_state(&connection).unwrap().unwrap();
            assert_initial_shape(&retained.readiness, CatalogReadinessPhase::Pending);
        }

        assert!(apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(expected, 22, 21),
        )
        .is_err());
        assert_eq!(counts(&connection), (1, 1, 1, 1));
    }

    #[test]
    fn registration_is_atomic_at_every_crash_seam_and_postcommit_retry_is_idempotent() {
        for stage in PRECOMMIT_STAGES {
            let mut connection = database();
            let result = apply_catalog_build_state_commit_with_hook(
                &mut connection,
                &register(plan()),
                &FailAt(stage),
            );
            assert!(
                matches!(result, Err(EngineError::InjectedFailure { .. })),
                "{stage:?}"
            );
            assert_eq!(counts(&connection), (0, 0, 0, 0), "{stage:?}");
            assert!(load_catalog_build_state(&connection).unwrap().is_none());
        }

        let mut connection = database();
        let result = apply_catalog_build_state_commit_with_hook(
            &mut connection,
            &register(plan()),
            &FailAt(CatalogCommitStage::AfterCommit),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(counts(&connection), (1, 1, 1, 1));
        let retained = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_initial_shape(&retained.readiness, CatalogReadinessPhase::Pending);
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &register(plan())).unwrap(),
            None
        );
        assert_eq!(counts(&connection), (1, 1, 1, 1));
    }

    #[test]
    fn scheduling_is_atomic_at_every_crash_seam_and_postcommit_retry_is_idempotent() {
        for stage in PRECOMMIT_STAGES {
            let mut connection = database();
            apply_catalog_build_state_commit(&mut connection, &register(plan()))
                .unwrap()
                .unwrap();
            let expected = load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .expectation()
                .unwrap();
            let schedule = CatalogBuildStateCommand::schedule(expected, 20, 21);
            let result = apply_catalog_build_state_commit_with_hook(
                &mut connection,
                &schedule,
                &FailAt(stage),
            );
            assert!(
                matches!(result, Err(EngineError::InjectedFailure { .. })),
                "{stage:?}"
            );
            assert_eq!(counts(&connection), (1, 1, 1, 1), "{stage:?}");
            let retained = load_catalog_build_state(&connection).unwrap().unwrap();
            assert_initial_shape(&retained.readiness, CatalogReadinessPhase::Pending);
        }

        let mut connection = database();
        apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        let expected = load_catalog_build_state(&connection)
            .unwrap()
            .unwrap()
            .expectation()
            .unwrap();
        let schedule = CatalogBuildStateCommand::schedule(expected, 20, 21);
        let result = apply_catalog_build_state_commit_with_hook(
            &mut connection,
            &schedule,
            &FailAt(CatalogCommitStage::AfterCommit),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(counts(&connection), (2, 1, 1, 2));
        let retained = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_initial_shape(&retained.readiness, CatalogReadinessPhase::Building);
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &schedule).unwrap(),
            None
        );
        assert_eq!(counts(&connection), (2, 1, 1, 2));
    }

    #[test]
    fn partial_progress_is_canonical_restart_safe_and_strictly_monotonic() {
        let (mut connection, plan) = scheduled_database();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        let expected = building.partial_expectation().unwrap();
        let first_coverage = vec![coverage(
            &plan,
            "claude-code",
            CoverageSetCompleteness::Complete,
        )];
        let first = CatalogBuildStateCommand::record_partial(
            expected.clone(),
            first_coverage.clone(),
            30,
            31,
        );
        let receipt = apply_catalog_build_state_commit(&mut connection, &first)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.commit_seq, 3);
        assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Partial);
        assert_eq!(receipt.readiness.source_coverage, first_coverage);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM catalog_partial_builds", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM catalog_partial_sources", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        let (schema_version, payload): (i64, Vec<u8>) = connection
            .query_row(
                "SELECT schema_version, payload FROM change_log WHERE commit_seq = 3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(schema_version, i64::from(PARTIAL_CHANGE_SCHEMA_VERSION));
        let payload_text = String::from_utf8(payload.clone()).unwrap();
        assert!(!payload_text.contains("claude-code"));
        assert!(!payload_text.contains("fixture/"));
        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload["state"], PARTIAL_STATE);
        assert_eq!(payload["source_count"], 1);
        assert!(payload.get("reason_code").is_none());

        let restarted = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(restarted.readiness, receipt.readiness);
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &first).unwrap(),
            None
        );

        let mut complete = first_coverage.clone();
        complete.push(coverage(&plan, "codex", CoverageSetCompleteness::Complete));
        complete.sort_by(|left, right| {
            (&left.scope.adapter_id, left.scope.source_instance_key)
                .cmp(&(&right.scope.adapter_id, right.scope.source_instance_key))
        });
        assert!(apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                restarted.partial_expectation().unwrap(),
                complete,
                40,
                41,
            ),
        )
        .is_err());
        assert!(apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                restarted.partial_expectation().unwrap(),
                first_coverage,
                40,
                41,
            ),
        )
        .is_err());
        assert!(apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                expected,
                vec![coverage(&plan, "grok", CoverageSetCompleteness::Complete,)],
                40,
                41,
            ),
        )
        .is_err());
        assert_eq!(
            load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness
                .state,
            CatalogReadinessPhase::Partial
        );
    }

    #[test]
    fn partial_progress_rolls_back_every_crash_seam_and_replays_lost_ack() {
        let stages = [
            CatalogCommitStage::BeforeTransaction,
            CatalogCommitStage::AfterCommitInsert,
            CatalogCommitStage::AfterPlanWrite,
            CatalogCommitStage::AfterPartialCoverageWrite,
            CatalogCommitStage::AfterBuildStateWrite,
            CatalogCommitStage::AfterOutboxInsert,
            CatalogCommitStage::BeforeCommit,
        ];
        for stage in stages {
            let (mut connection, plan) = scheduled_database();
            let building = load_catalog_build_state(&connection).unwrap().unwrap();
            let command = CatalogBuildStateCommand::record_partial(
                building.partial_expectation().unwrap(),
                vec![coverage(
                    &plan,
                    "claude-code",
                    CoverageSetCompleteness::Complete,
                )],
                30,
                31,
            );
            let result = apply_catalog_build_state_commit_with_hook(
                &mut connection,
                &command,
                &FailAt(stage),
            );
            assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
            assert_eq!(
                load_catalog_build_state(&connection)
                    .unwrap()
                    .unwrap()
                    .readiness
                    .state,
                CatalogReadinessPhase::Building,
                "{stage:?}"
            );
            assert_eq!(
                connection
                    .query_row("SELECT COUNT(*) FROM catalog_partial_builds", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                0,
                "{stage:?}"
            );
        }

        let (mut connection, plan) = scheduled_database();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        let command = CatalogBuildStateCommand::record_partial(
            building.partial_expectation().unwrap(),
            vec![coverage(
                &plan,
                "claude-code",
                CoverageSetCompleteness::Complete,
            )],
            30,
            31,
        );
        let result = apply_catalog_build_state_commit_with_hook(
            &mut connection,
            &command,
            &FailAt(CatalogCommitStage::AfterCommit),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &command).unwrap(),
            None
        );
        assert_eq!(
            load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness
                .state,
            CatalogReadinessPhase::Partial
        );
    }

    #[test]
    fn partial_restart_rejects_payload_chain_and_terminal_anchor_corruption() {
        let corrupt = |statement: &str| {
            let (mut connection, plan) = scheduled_database();
            let building = load_catalog_build_state(&connection).unwrap().unwrap();
            apply_catalog_build_state_commit(
                &mut connection,
                &CatalogBuildStateCommand::record_partial(
                    building.partial_expectation().unwrap(),
                    vec![coverage(
                        &plan,
                        "claude-code",
                        CoverageSetCompleteness::Complete,
                    )],
                    30,
                    31,
                ),
            )
            .unwrap()
            .unwrap();
            connection
                .execute_batch(
                    "DROP TRIGGER catalog_partial_builds_no_update; \
                     DROP TRIGGER catalog_partial_sources_no_update; \
                     PRAGMA foreign_keys = OFF; \
                     PRAGMA ignore_check_constraints = ON;",
                )
                .unwrap();
            connection.execute_batch(statement).unwrap();
            load_catalog_build_state(&connection).unwrap_err()
        };

        assert!(
            corrupt("UPDATE catalog_partial_sources SET payload_digest = zeroblob(32);")
                .to_string()
                .contains("payload digest")
        );
        assert!(
            corrupt("UPDATE catalog_partial_builds SET predecessor_state_commit_seq = 1;")
                .to_string()
                .contains("foreign build owner")
        );
        assert!(
            corrupt("UPDATE change_log SET payload = x'7b7d' WHERE commit_seq = 2;")
                .to_string()
                .contains("base invalidation")
        );
        assert!(corrupt(
            "UPDATE catalog_build_state SET state = 'building', last_commit_seq = 2, updated_at = 21;"
        )
        .to_string()
        .contains("orphaned"));
    }

    #[test]
    fn partial_initial_integrity_failure_anchors_progress_and_retries_a_new_attempt() {
        let (mut connection, plan) = scheduled_database();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                building.partial_expectation().unwrap(),
                vec![coverage(
                    &plan,
                    "claude-code",
                    CoverageSetCompleteness::Complete,
                )],
                30,
                31,
            ),
        )
        .unwrap()
        .unwrap();
        let partial = load_catalog_build_state(&connection).unwrap().unwrap();
        let failed = apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::fail_initial_build_integrity(
                partial.initial_integrity_expectation().unwrap(),
                "partial_projection_invalid",
                40,
                41,
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(failed.readiness.state, CatalogReadinessPhase::Error);
        let restarted = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(restarted.readiness, failed.readiness);
        let retried = apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::retry_terminal_refresh(
                restarted.expectation().unwrap(),
                50,
                51,
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(retried.readiness.state, CatalogReadinessPhase::Building);
        assert_eq!(retried.readiness.attempt, 2);
        let recovered = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(
            recovered.recovery_origin,
            Some(CatalogDurableBuildPhase::Error)
        );
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                recovered.partial_expectation().unwrap(),
                vec![coverage(
                    &plan,
                    "claude-code",
                    CoverageSetCompleteness::Complete,
                )],
                60,
                61,
            ),
        )
        .unwrap()
        .unwrap();
        let recovered_partial = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(
            recovered_partial.readiness.state,
            CatalogReadinessPhase::Partial
        );
        assert_eq!(recovered_partial.readiness.attempt, 2);
        assert_eq!(
            recovered_partial.recovery_origin,
            Some(CatalogDurableBuildPhase::Error)
        );
    }

    #[test]
    fn discarded_initial_integrity_failure_restarts_and_opens_a_new_attempt() {
        let mut connection = database();
        apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        let pending = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();

        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        let fail = CatalogBuildStateCommand::fail_initial_build_integrity(
            building.initial_integrity_expectation().unwrap(),
            "initial_projection_invalid",
            30,
            31,
        );
        let failed = apply_catalog_build_state_commit(&mut connection, &fail)
            .unwrap()
            .unwrap();
        assert_eq!(failed.readiness.state, CatalogReadinessPhase::Error);
        assert_eq!(failed.readiness.attempt, 1);
        assert_eq!(failed.readiness.completed_contract_version, None);
        assert_eq!(failed.readiness.last_complete_snapshot, None);
        assert!(failed.readiness.source_coverage.is_empty());
        assert_eq!(
            failed.readiness.reason,
            Some(CatalogReadinessReason::IntegrityFailure {
                code: "initial_projection_invalid".to_string(),
                snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
            })
        );
        assert!(load_catalog_build_state(&connection)
            .unwrap()
            .unwrap()
            .ready_read_authority()
            .is_err());
        let (disposition, retained_snapshot, reason, schema_version): (
            String,
            Option<i64>,
            String,
            i64,
        ) = connection
            .query_row(
                r#"
                SELECT failure.snapshot_disposition,
                       failure.retained_snapshot_commit_seq,
                       owner.reason,
                       change.schema_version
                FROM catalog_refresh_integrity_failures AS failure
                JOIN ingest_commits AS owner
                  ON owner.commit_seq = failure.failure_commit_seq
                JOIN change_log AS change
                  ON change.commit_seq = failure.failure_commit_seq
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(disposition, "discarded");
        assert_eq!(retained_snapshot, None);
        assert_eq!(reason, INITIAL_INTEGRITY_FAILURE_REASON);
        assert_eq!(
            schema_version,
            i64::from(INITIAL_INTEGRITY_FAILURE_CHANGE_SCHEMA_VERSION)
        );
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &fail).unwrap(),
            None
        );

        let failed = load_catalog_build_state(&connection).unwrap().unwrap();
        let failed_expectation = failed.expectation().unwrap();
        let retry =
            CatalogBuildStateCommand::retry_terminal_refresh(failed_expectation.clone(), 40, 41);
        let retried = apply_catalog_build_state_commit(&mut connection, &retry)
            .unwrap()
            .unwrap();
        assert_eq!(retried.readiness.state, CatalogReadinessPhase::Building);
        assert_eq!(retried.readiness.attempt, 2);
        assert_eq!(retried.readiness.last_complete_snapshot, None);
        assert!(retried.readiness.source_coverage.is_empty());
        let restarted = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(restarted.readiness, retried.readiness);
        assert_eq!(
            restarted.recovery_origin,
            Some(CatalogDurableBuildPhase::Error)
        );
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &retry).unwrap(),
            None
        );
        let mut forged_origin = failed_expectation;
        forged_origin.state = CatalogDurableBuildPhase::Degraded;
        assert!(apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::retry_terminal_refresh(forged_origin, 40, 41),
        )
        .is_err());

        let second_failure = CatalogBuildStateCommand::fail_initial_build_integrity(
            restarted.initial_integrity_expectation().unwrap(),
            "second_projection_invalid",
            50,
            51,
        );
        apply_catalog_build_state_commit(&mut connection, &second_failure)
            .unwrap()
            .unwrap();
        let failure_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM catalog_refresh_integrity_failures",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failure_count, 2);
        let second = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(second.readiness.state, CatalogReadinessPhase::Error);
        assert_eq!(second.readiness.attempt, 2);
    }

    #[test]
    fn discarded_initial_integrity_failure_is_atomic_at_every_crash_seam() {
        let stages = [
            CatalogCommitStage::BeforeTransaction,
            CatalogCommitStage::AfterCommitInsert,
            CatalogCommitStage::AfterPlanWrite,
            CatalogCommitStage::AfterFailureEvidenceWrite,
            CatalogCommitStage::AfterBuildStateWrite,
            CatalogCommitStage::AfterOutboxInsert,
            CatalogCommitStage::BeforeCommit,
        ];
        for stage in stages {
            let mut connection = database();
            apply_catalog_build_state_commit(&mut connection, &register(plan()))
                .unwrap()
                .unwrap();
            let pending = load_catalog_build_state(&connection).unwrap().unwrap();
            apply_catalog_build_state_commit(
                &mut connection,
                &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
            )
            .unwrap()
            .unwrap();
            let building = load_catalog_build_state(&connection).unwrap().unwrap();
            let command = CatalogBuildStateCommand::fail_initial_build_integrity(
                building.initial_integrity_expectation().unwrap(),
                "initial_projection_invalid",
                30,
                31,
            );
            let result = apply_catalog_build_state_commit_with_hook(
                &mut connection,
                &command,
                &FailAt(stage),
            );
            assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
            let retained = load_catalog_build_state(&connection).unwrap().unwrap();
            assert_eq!(retained.readiness.state, CatalogReadinessPhase::Building);
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM catalog_refresh_integrity_failures",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "{stage:?}",
            );
        }

        let mut connection = database();
        apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        let pending = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        let command = CatalogBuildStateCommand::fail_initial_build_integrity(
            building.initial_integrity_expectation().unwrap(),
            "initial_projection_invalid",
            30,
            31,
        );
        let result = apply_catalog_build_state_commit_with_hook(
            &mut connection,
            &command,
            &FailAt(CatalogCommitStage::AfterCommit),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &command).unwrap(),
            None
        );
        assert_eq!(
            load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness
                .state,
            CatalogReadinessPhase::Error
        );
    }

    #[test]
    fn discarded_initial_integrity_restart_rejects_hybrid_evidence() {
        let mut connection = database();
        apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        let pending = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::fail_initial_build_integrity(
                building.initial_integrity_expectation().unwrap(),
                "initial_projection_invalid",
                30,
                31,
            ),
        )
        .unwrap()
        .unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER catalog_refresh_integrity_failures_no_update; PRAGMA ignore_check_constraints = ON;",
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_refresh_integrity_failures SET snapshot_disposition = 'independently_safe'",
                [],
            )
            .unwrap();
        let error = load_catalog_build_state(&connection).unwrap_err();
        assert!(error
            .to_string()
            .contains("integrity-failure ledger is not one canonical sequence"));
    }

    #[test]
    fn source_generation_invalidation_is_atomic_restart_safe_and_idempotent() {
        let stages = [
            CatalogCommitStage::BeforeTransaction,
            CatalogCommitStage::AfterCommitInsert,
            CatalogCommitStage::AfterPlanWrite,
            CatalogCommitStage::AfterEpochInvalidationWrite,
            CatalogCommitStage::AfterBuildStateWrite,
            CatalogCommitStage::AfterOutboxInsert,
            CatalogCommitStage::BeforeCommit,
        ];
        for stage in stages {
            let (mut connection, _) = scheduled_database();
            let building = load_catalog_build_state(&connection).unwrap().unwrap();
            let command = CatalogBuildStateCommand::invalidate_source_generation(
                building
                    .source_generation_invalidation_expectation()
                    .unwrap(),
                30,
                31,
            );
            let result = apply_catalog_build_state_commit_with_hook(
                &mut connection,
                &command,
                &FailAt(stage),
            );
            assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
            let retained = load_catalog_build_state(&connection).unwrap().unwrap();
            assert_initial_shape(&retained.readiness, CatalogReadinessPhase::Building);
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM catalog_epoch_invalidations",
                        [],
                        |row| { row.get::<_, i64>(0) }
                    )
                    .unwrap(),
                0,
                "{stage:?}",
            );
        }

        let (mut connection, plan) = scheduled_database();
        let building = load_catalog_build_state(&connection).unwrap().unwrap();
        let first_expectation = building
            .source_generation_invalidation_expectation()
            .unwrap();
        let first = CatalogBuildStateCommand::invalidate_source_generation(
            first_expectation.clone(),
            30,
            31,
        );
        let lost_ack = apply_catalog_build_state_commit_with_hook(
            &mut connection,
            &first,
            &FailAt(CatalogCommitStage::AfterCommit),
        );
        assert!(matches!(lost_ack, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(
            apply_catalog_build_state_commit(&mut connection, &first).unwrap(),
            None
        );

        let replacement = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(replacement.readiness.state, CatalogReadinessPhase::Building);
        assert_eq!(replacement.readiness.epoch, 2);
        assert_eq!(replacement.readiness.attempt, 1);
        assert_eq!(replacement.readiness.last_complete_snapshot, None);
        assert!(replacement.epoch_replacement);
        let (previous_epoch, epoch, previous_attempt, previous_state): (i64, i64, i64, String) =
            connection
                .query_row(
                    "SELECT previous_epoch, epoch, previous_attempt, previous_state FROM catalog_epoch_invalidations",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
        assert_eq!((previous_epoch, epoch, previous_attempt), (1, 2, 1));
        assert_eq!(previous_state, BUILDING_STATE);
        let (schema_version, payload): (i64, Vec<u8>) = connection
            .query_row(
                "SELECT schema_version, payload FROM change_log WHERE commit_seq = 3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            schema_version,
            i64::from(EPOCH_INVALIDATION_CHANGE_SCHEMA_VERSION)
        );
        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload["previous_epoch"], 1);
        assert_eq!(payload["epoch"], 2);
        assert_eq!(payload["attempt"], 1);
        assert_eq!(payload["state"], BUILDING_STATE);

        let partial = CatalogBuildStateCommand::record_partial(
            replacement.partial_expectation().unwrap(),
            vec![coverage(
                &plan,
                "claude-code",
                CoverageSetCompleteness::Complete,
            )],
            40,
            41,
        );
        apply_catalog_build_state_commit(&mut connection, &partial)
            .unwrap()
            .unwrap();
        let partial_state = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(
            partial_state.readiness.state,
            CatalogReadinessPhase::Partial
        );
        assert_eq!(partial_state.readiness.epoch, 2);
        let second = CatalogBuildStateCommand::invalidate_source_generation(
            partial_state
                .source_generation_invalidation_expectation()
                .unwrap(),
            50,
            51,
        );
        apply_catalog_build_state_commit(&mut connection, &second)
            .unwrap()
            .unwrap();
        let restarted = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_eq!(restarted.readiness.state, CatalogReadinessPhase::Building);
        assert_eq!(restarted.readiness.epoch, 3);
        assert_eq!(restarted.readiness.attempt, 1);
        assert!(restarted.readiness.source_coverage.is_empty());
        assert!(restarted.epoch_replacement);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM catalog_epoch_invalidations",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .unwrap(),
            2,
        );
        assert!(apply_catalog_build_state_commit(&mut connection, &first).is_err());
    }

    #[test]
    fn source_generation_invalidation_restart_rejects_ledger_corruption() {
        let corrupt = |statement: &str| {
            let (mut connection, _) = scheduled_database();
            let state = load_catalog_build_state(&connection).unwrap().unwrap();
            apply_catalog_build_state_commit(
                &mut connection,
                &CatalogBuildStateCommand::invalidate_source_generation(
                    state.source_generation_invalidation_expectation().unwrap(),
                    30,
                    31,
                ),
            )
            .unwrap()
            .unwrap();
            connection
                .execute_batch(
                    "DROP TRIGGER catalog_epoch_invalidations_no_update; \
                     PRAGMA foreign_keys = OFF; \
                     PRAGMA ignore_check_constraints = ON;",
                )
                .unwrap();
            connection.execute_batch(statement).unwrap();
            load_catalog_build_state(&connection).unwrap_err()
        };

        assert!(
            corrupt("UPDATE catalog_epoch_invalidations SET previous_epoch = 2;")
                .to_string()
                .contains("exact administrative owner")
        );
        let predecessor_error =
            corrupt("UPDATE catalog_epoch_invalidations SET predecessor_state_commit_seq = 1;");
        assert!(
            predecessor_error
                .to_string()
                .contains("exact durable change"),
            "{predecessor_error}"
        );
        assert!(
            corrupt("UPDATE change_log SET payload = x'7b7d' WHERE commit_seq = 3;")
                .to_string()
                .contains("exact durable change")
        );
        let owner_error = corrupt(
            "UPDATE ingest_commits SET reason = 'catalog.library.build.scheduled' WHERE commit_seq = 3;",
        );
        assert!(
            owner_error
                .to_string()
                .contains("exact administrative owner"),
            "{owner_error}"
        );
    }

    #[test]
    fn restart_reconstructs_the_b1_machine_and_rejects_corrupt_plan_bytes() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("catalog-state.db");
        {
            let mut connection = Connection::open(&database_path).unwrap();
            schema::initialize_schema(&connection).unwrap();
            apply_catalog_build_state_commit(&mut connection, &register(plan()))
                .unwrap()
                .unwrap();
        }
        {
            let mut connection = Connection::open(&database_path).unwrap();
            schema::initialize_schema(&connection).unwrap();
            let pending = load_catalog_build_state(&connection).unwrap().unwrap();
            assert_initial_shape(&pending.readiness, CatalogReadinessPhase::Pending);
            let schedule =
                CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21);
            apply_catalog_build_state_commit(&mut connection, &schedule)
                .unwrap()
                .unwrap();
        }
        {
            let connection = Connection::open(&database_path).unwrap();
            schema::initialize_schema(&connection).unwrap();
            let building = load_catalog_build_state(&connection).unwrap().unwrap();
            assert_initial_shape(&building.readiness, CatalogReadinessPhase::Building);
            connection
                .execute(
                    "UPDATE catalog_coverage_plans SET content_digest = zeroblob(32)",
                    [],
                )
                .unwrap();
            assert!(load_catalog_build_state(&connection).is_err());
        }

        match WriterRuntime::start(database_path) {
            Err(EngineError::InvalidCommit(message)) => {
                assert!(message.contains("content digest"));
            }
            Err(other) => panic!("unexpected writer restart error: {other}"),
            Ok(mut runtime) => {
                runtime.shutdown().unwrap();
                panic!("corrupt catalog state unexpectedly started the writer");
            }
        }
    }

    #[test]
    fn restart_projects_identity_phase_and_commit_reasons_before_decoding() {
        let mut connection = database();
        apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        let pending = load_catalog_build_state(&connection).unwrap().unwrap();
        apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();

        let original_plan_id = plan().coverage_plan_id.storage_bytes().to_vec();
        let oversized_plan_id = vec![7_u8; 33];
        connection
            .execute(
                "UPDATE catalog_coverage_plans SET coverage_plan_id = ?1",
                [&oversized_plan_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_build_state SET coverage_plan_id = ?1",
                [&oversized_plan_id],
            )
            .unwrap();
        let error = load_catalog_build_state(&connection).unwrap_err();
        assert!(error.to_string().contains("plan identity exceeds"));
        connection
            .execute(
                "UPDATE catalog_coverage_plans SET coverage_plan_id = ?1",
                [&original_plan_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_build_state SET coverage_plan_id = ?1",
                [&original_plan_id],
            )
            .unwrap();

        let original_content_digest: Vec<u8> = connection
            .query_row(
                "SELECT content_digest FROM catalog_coverage_plans",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_coverage_plans SET content_digest = ?1",
                [vec![8_u8; 33]],
            )
            .unwrap();
        let error = load_catalog_build_state(&connection).unwrap_err();
        assert!(error
            .to_string()
            .contains("content digest exceeds its fixed durable bound"));
        connection
            .execute(
                "UPDATE catalog_coverage_plans SET content_digest = ?1",
                [original_content_digest],
            )
            .unwrap();

        connection
            .execute(
                "UPDATE catalog_build_state SET state = ?1",
                ["x".repeat(257)],
            )
            .unwrap();
        let error = load_catalog_build_state(&connection).unwrap_err();
        assert!(error.to_string().contains("closed vocabulary"));
        connection
            .execute(
                "UPDATE catalog_build_state SET state = ?1",
                [BUILDING_STATE],
            )
            .unwrap();

        connection
            .execute(
                "UPDATE ingest_commits SET reason = ?1 WHERE commit_seq = 1",
                ["x".repeat(257)],
            )
            .unwrap();
        let error = load_catalog_build_state(&connection).unwrap_err();
        assert!(error
            .to_string()
            .contains("plan commit reason is outside its closed vocabulary"));
        connection
            .execute(
                "UPDATE ingest_commits SET reason = ?1 WHERE commit_seq = 1",
                [REGISTER_REASON],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE ingest_commits SET reason = ?1 WHERE commit_seq = 2",
                ["x".repeat(257)],
            )
            .unwrap();
        let error = load_catalog_build_state(&connection).unwrap_err();
        assert!(error
            .to_string()
            .contains("state commit reason is outside its closed vocabulary"));
        connection
            .execute(
                "UPDATE ingest_commits SET reason = ?1 WHERE commit_seq = 2",
                [SCHEDULE_REASON],
            )
            .unwrap();
        assert_eq!(
            load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness
                .state,
            CatalogReadinessPhase::Building
        );
    }

    #[test]
    fn restart_rejects_unreconstructable_epoch_and_foreign_commit_ownership() {
        let mut connection = database();
        apply_catalog_build_state_commit(&mut connection, &register(plan()))
            .unwrap()
            .unwrap();
        let forged_json = serde_json::to_vec(&other_plan()).unwrap();
        let forged_digest = blake3::hash(&forged_json);
        connection
            .execute(
                r#"
                UPDATE catalog_coverage_plans
                SET plan_json = ?1, content_digest = ?2
                "#,
                params![forged_json, forged_digest.as_bytes().as_slice()],
            )
            .unwrap();
        assert!(load_catalog_build_state(&connection).is_err());
        let original_json = serde_json::to_vec(&plan()).unwrap();
        let original_digest = blake3::hash(&original_json);
        connection
            .execute(
                r#"
                UPDATE catalog_coverage_plans
                SET plan_json = ?1, content_digest = ?2
                "#,
                params![original_json, original_digest.as_bytes().as_slice()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_build_state SET epoch = 2 WHERE scope_kind = 'library'",
                [],
            )
            .unwrap();
        assert!(load_catalog_build_state(&connection).is_err());
        connection
            .execute(
                "UPDATE catalog_build_state SET epoch = 1 WHERE scope_kind = 'library'",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_build_state SET updated_at = updated_at + 1 WHERE scope_kind = 'library'",
                [],
            )
            .unwrap();
        assert!(load_catalog_build_state(&connection).is_err());
        connection
            .execute(
                "UPDATE catalog_build_state SET updated_at = updated_at - 1 WHERE scope_kind = 'library'",
                [],
            )
            .unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO source_instances (
                    source_instance_id, adapter_id, stable_key, display_name,
                    adapter_version, adapter_contract_version,
                    source_schema_versions_json, capabilities_json,
                    discovered_at, last_seen_at
                ) VALUES (99, 'fixture', x'63', 'fixture', '1', 1, '[]', '[]', 1, 1);
                UPDATE ingest_commits SET source_instance_id = 99 WHERE commit_seq = 1;
                "#,
            )
            .unwrap();
        assert!(load_catalog_build_state(&connection).is_err());
    }

    #[test]
    fn engine_writer_publishes_only_new_catalog_commit_watermarks() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("catalog-engine.db");
        let engine = SpaghettiEngineCore::open(EngineOptions {
            database_path: database_path.clone(),
            query_workers: Some(1),
            owner_label: Some("catalog-state-test".to_string()),
            defer_query_structures: false,
            source_pass_pool: None,
        })
        .unwrap();
        let plan = plan();
        assert_eq!(engine.latest_commit_seq(), 0);
        assert_eq!(
            engine
                .commit_catalog_build_state(register(plan.clone()))
                .unwrap(),
            Some(1)
        );
        assert_eq!(engine.latest_commit_seq(), 1);
        assert_eq!(
            engine
                .commit_catalog_build_state(register(plan.clone()))
                .unwrap(),
            None
        );
        assert_eq!(engine.latest_commit_seq(), 1);
        let expectation = CatalogBuildExpectation {
            scope: CatalogCoverageScope::Library,
            coverage_plan_id: plan.coverage_plan_id,
            desired_contract_version: 1,
            epoch: 1,
            attempt: 1,
            state: CatalogDurableBuildPhase::Pending,
            state_commit_seq: 1,
        };
        assert_eq!(
            engine
                .commit_catalog_build_state(
                    CatalogBuildStateCommand::schedule(expectation, 20, 21,)
                )
                .unwrap(),
            Some(2)
        );
        assert_eq!(engine.latest_commit_seq(), 2);
        engine.shutdown().unwrap();

        let connection = Connection::open(database_path).unwrap();
        schema::initialize_schema(&connection).unwrap();
        let retained = load_catalog_build_state(&connection).unwrap().unwrap();
        assert_initial_shape(&retained.readiness, CatalogReadinessPhase::Building);
        assert_eq!(retained.last_commit_seq, 2);
    }
}
