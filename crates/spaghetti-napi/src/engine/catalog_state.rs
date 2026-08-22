//! Source-neutral RFC 012B Library plan and initial build-state durability.
//!
//! This module owns `Pending`/`Building` administration, reconstructs the
//! initial `Ready` snapshot published atomically by `catalog_publication`, and
//! can durably begin an ordinary refresh while retaining that exact snapshot.
//! An active refresh may also fail with exact independently-safe integrity
//! evidence while the prior publication remains queryable. It still owns no
//! source reads, refresh completion/retirement policy, or public query
//! authority.

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::adapter::{CoverageSetCompleteness, SourceCoverageSet};
use crate::catalog_contract::evidence::{CatalogReducer, CatalogReducerPublication};
use crate::catalog_contract::publication::{
    CatalogPublicationMemberHistory, CatalogRefreshPredecessor,
};
use crate::catalog_contract::{
    validate_reason_code, CatalogCoveragePlan, CatalogCoveragePlanId, CatalogCoverageScope,
    CatalogIntegritySnapshotDisposition, CatalogReadinessMachine, CatalogReadinessPhase,
    CatalogReadinessReason, CatalogReadinessSnapshot, CatalogSnapshotId,
    CATALOG_READINESS_CONTRACT_VERSION,
};

use super::catalog_publication::CatalogReadyPublicationIdentity;
use super::commit::{self, ChangeEntry};
use super::EngineError;

pub(super) const LIBRARY_SCOPE: &str = "library";
const PENDING_STATE: &str = "pending";
const BUILDING_STATE: &str = "building";
const READY_STATE: &str = "ready";
const DEGRADED_STATE: &str = "degraded";
const ERROR_STATE: &str = "error";
const REGISTER_REASON: &str = "catalog.library.plan.registered";
const SCHEDULE_REASON: &str = "catalog.library.build.scheduled";
pub(super) const INITIAL_PUBLICATION_REASON: &str = "catalog.library.initial_snapshot.published";
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
const MAX_CATALOG_PLAN_JSON_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogDurableBuildPhase {
    Pending,
    Building,
    Ready,
    Degraded,
    Error,
}

impl CatalogDurableBuildPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => PENDING_STATE,
            Self::Building => BUILDING_STATE,
            Self::Ready => READY_STATE,
            Self::Degraded => DEGRADED_STATE,
            Self::Error => ERROR_STATE,
        }
    }

    fn parse(value: &str) -> Result<Self, EngineError> {
        match value {
            PENDING_STATE => Ok(Self::Pending),
            BUILDING_STATE => Ok(Self::Building),
            READY_STATE => Ok(Self::Ready),
            DEGRADED_STATE => Ok(Self::Degraded),
            ERROR_STATE => Ok(Self::Error),
            _ => Err(corrupt_catalog_state(format!(
                "unsupported durable build state {value:?}"
            ))),
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
        self.lineage == CatalogRefreshExecutionLineage::RecoveryBuilding
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
    BeginRefresh {
        expected: CatalogReadyRefreshExpectation,
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
    RetryDegradedRefresh {
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

    pub(crate) fn retry_degraded_refresh(
        expected: CatalogBuildExpectation,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self::RetryDegradedRefresh {
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
            (CatalogReadinessPhase::Building, None, None)
            | (
                CatalogReadinessPhase::Building,
                None,
                Some(CatalogReadinessReason::SourceRetrying { .. }),
            ) => {
                self.readiness.complete_through_commit.is_none()
                    && self.readiness.attempt
                        > self
                            .ready_publication_attempt
                            .unwrap_or(self.readiness.attempt)
                    && self.last_commit_seq > snapshot_id.complete_commit
            }
            _ => false,
        };
        let completion_lineage_is_exact = self.readiness.complete_through_commit
            == Some(snapshot_id.complete_commit)
            || (self.readiness.complete_through_commit.is_none()
                && matches!(
                    self.readiness.state,
                    CatalogReadinessPhase::Building | CatalogReadinessPhase::Degraded
                ));
        if self.plan.scope != CatalogCoverageScope::Library
            || self.readiness.coverage_plan_id != self.plan.coverage_plan_id
            || self.readiness.completed_contract_version != Some(snapshot_id.pack_contract_version)
            || !completion_lineage_is_exact
            || self.readiness.epoch != snapshot_id.readiness_epoch
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
            CatalogReadinessPhase::Building
                if self.readiness.refreshing_from_snapshot.is_none()
                    && self.readiness.complete_through_commit.is_none()
                    && matches!(
                        self.readiness.reason.as_ref(),
                        None | Some(CatalogReadinessReason::SourceRetrying { .. })
                    )
                    && self.readiness.completed_contract_version
                        == Some(predecessor_snapshot.pack_contract_version)
                    && self.readiness.attempt
                        > self
                            .ready_publication_attempt
                            .unwrap_or(self.readiness.attempt) =>
            {
                CatalogRefreshExecutionLineage::RecoveryBuilding
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
        historical_readiness.completed_contract_version = Some(snapshot_id.pack_contract_version);
        historical_readiness.attempt = historical_attempt;
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
    complete_through_commit: u64,
    reason_code: &'a str,
    snapshot_disposition: CatalogIntegritySnapshotDisposition,
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
    Schedule,
    BeginRefresh {
        expected: &'a CatalogReadyRefreshExpectation,
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
    RetryDegradedRefresh,
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
                CatalogBuildStateWrite::Schedule,
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
            machine
                .source_retrying(
                    reason_code.clone(),
                    current.readiness.source_coverage.clone(),
                )
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
            let source_coverage = unavailable_source_coverage(&current.readiness.source_coverage)?;
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
        CatalogBuildStateCommand::RetryDegradedRefresh {
            expected,
            started_at,
            committed_at,
        } => {
            let Some(current) = current else {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery requires durable degraded readiness".to_string(),
                ));
            };
            let actual = current.expectation()?;
            if actual.state == CatalogDurableBuildPhase::Building {
                let already_applied = CatalogBuildExpectation {
                    state: CatalogDurableBuildPhase::Degraded,
                    attempt: actual.attempt.checked_sub(1).ok_or_else(|| {
                        EngineError::InvalidCommit(
                            "catalog recovery attempt cannot be decremented".to_string(),
                        )
                    })?,
                    ..actual
                };
                if *expected == already_applied {
                    transaction.commit().map_err(|error| {
                        sqlite_error("finish unchanged catalog recovery start", error)
                    })?;
                    return Ok(None);
                }
            }
            if expected.state != CatalogDurableBuildPhase::Degraded || *expected != actual {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery compare-and-swap expectation is stale or foreign".to_string(),
                ));
            }
            let mut machine =
                CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
                    .map_err(catalog_contract_error)?;
            machine.retry().map_err(catalog_contract_error)?;
            (
                machine,
                *started_at,
                *committed_at,
                REFRESH_RECOVERY_STARTED_REASON,
                CatalogBuildStateWrite::RetryDegradedRefresh,
            )
        }
    };

    let commit_seq = insert_administrative_commit(&transaction, reason, started_at, committed_at)?;
    hook.reach(CatalogCommitStage::AfterCommitInsert)?;
    if matches!(write, CatalogBuildStateWrite::InsertPlan) {
        insert_plan(&transaction, machine.plan(), commit_seq)?;
    }
    hook.reach(CatalogCommitStage::AfterPlanWrite)?;
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
    write_readiness_change(&transaction, commit_seq, machine.snapshot())?;
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
                                  AND build.state IN ('pending', 'building', 'ready', 'degraded', 'error')
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
                                      'catalog.library.initial_snapshot.published',
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
                CatalogDurableBuildPhase::Ready | CatalogDurableBuildPhase::Building
            )
        {
            REFRESH_SOURCE_RETRYING_REASON
        } else if phase == CatalogDurableBuildPhase::Degraded {
            REFRESH_SOURCE_UNAVAILABLE_REASON
        } else if phase == CatalogDurableBuildPhase::Error {
            REFRESH_INTEGRITY_FAILURE_REASON
        } else if phase == CatalogDurableBuildPhase::Building
            && state_commit_reason == REFRESH_RECOVERY_STARTED_REASON
        {
            REFRESH_RECOVERY_STARTED_REASON
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
        CatalogDurableBuildPhase::Degraded
            if completed_contract_version != Some(desired_contract_version)
                || complete_through_commit
                    .is_some_and(|commit| Some(commit) != last_complete_snapshot_commit)
                || refreshing_from_snapshot_commit.is_some()
                || last_complete_snapshot_commit.is_none_or(|commit| last_commit_seq <= commit) =>
        {
            return Err(corrupt_catalog_state(
                "degraded catalog readiness does not retain one exact prior snapshot",
            ));
        }
        CatalogDurableBuildPhase::Error
            if completed_contract_version != Some(desired_contract_version)
                || complete_through_commit != last_complete_snapshot_commit
                || complete_through_commit.is_none()
                || refreshing_from_snapshot_commit.is_some()
                || complete_through_commit.is_some_and(|commit| last_commit_seq <= commit) =>
        {
            return Err(corrupt_catalog_state(
                "independently-safe catalog Error does not retain one exact prior snapshot",
            ));
        }
        _ => {}
    }
    let reason_shape_is_valid = match phase {
        CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Error => {
            stored_reason_code.is_some()
        }
        CatalogDurableBuildPhase::Building => {
            last_complete_snapshot_commit.is_some() || stored_reason_code.is_none()
        }
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
        retired_snapshot_count,
    ) = if let Some(snapshot_commit) = last_complete_snapshot_commit {
        let snapshot_id = CatalogSnapshotId::new(
            desired_contract_version,
            plan.coverage_plan_id,
            epoch,
            snapshot_commit,
        )
        .map_err(catalog_contract_error)?;
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
        let (snapshot_state, source_coverage, readiness_reason) = if phase
            == CatalogDurableBuildPhase::Error
        {
            let failure = load_integrity_failure_evidence(connection)?.ok_or_else(|| {
                corrupt_catalog_state(
                    "catalog Error is missing its exact integrity-failure evidence",
                )
            })?;
            let evidence_reason = validate_integrity_failure_for_restart(
                &failure,
                CatalogIntegrityRestartContext {
                    plan: &plan,
                    epoch,
                    attempt,
                    state_commit_seq: last_commit_seq,
                    updated_at: stored.updated_at,
                    retained_snapshot: snapshot_id,
                    publication_identity: &publication.identity,
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
                    snapshot_disposition: CatalogIntegritySnapshotDisposition::IndependentlySafe,
                }),
            )
        } else if matches!(
            phase,
            CatalogDurableBuildPhase::Degraded | CatalogDurableBuildPhase::Building
        ) {
            let failure = source_failure.as_ref().ok_or_else(|| {
                corrupt_catalog_state(
                    "catalog degraded/recovery state is missing source-failure evidence",
                )
            })?;
            let recovering = phase == CatalogDurableBuildPhase::Building;
            let (_, evidence_reason) = validate_source_failure_for_restart(
                failure,
                CatalogSourceFailureRestartContext {
                    plan: &plan,
                    state_attempt: attempt,
                    state_commit_seq: last_commit_seq,
                    updated_at: stored.updated_at,
                    retained_snapshot: snapshot_id,
                    publication_identity: &publication.identity,
                    publication_attempt,
                    recovering,
                },
            )?;
            if !recovering && stored_reason_code != Some(evidence_reason.as_str()) {
                return Err(corrupt_catalog_state(
                    "catalog degraded reason differs from source-failure evidence",
                ));
            }
            let recovery_reason =
                stored_reason_code.map(|code| CatalogReadinessReason::SourceRetrying {
                    code: code.to_string(),
                });
            (
                if recovering {
                    CatalogReadinessPhase::Building
                } else {
                    CatalogReadinessPhase::Degraded
                },
                unavailable_source_coverage(&publication_coverage)?,
                if recovering {
                    recovery_reason
                } else {
                    Some(CatalogReadinessReason::TerminalSourceUnavailable {
                        code: evidence_reason,
                    })
                },
            )
        } else {
            if load_integrity_failure_evidence(connection)?.is_some() {
                return Err(corrupt_catalog_state(
                    "non-error catalog state contains integrity-failure evidence",
                ));
            }
            if publication_attempt != attempt {
                return Err(corrupt_catalog_state(
                    "Ready catalog attempt differs from its publication attempt",
                ));
            }
            let ready_reason =
                stored_reason_code.map(|code| CatalogReadinessReason::SourceRetrying {
                    code: code.to_string(),
                });
            (
                CatalogReadinessPhase::Ready,
                publication_coverage.clone(),
                ready_reason,
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
            refreshing_from_snapshot: refreshing_from_snapshot_commit.map(|commit| {
                CatalogSnapshotId::new(
                    desired_contract_version,
                    plan.coverage_plan_id,
                    epoch,
                    commit,
                )
                .expect("validated retained refresh snapshot")
            }),
            source_coverage,
            reason: readiness_reason,
        };
        machine = CatalogReadinessMachine::resume(plan.clone(), reconstructed)
            .map_err(catalog_contract_error)?;
        (
            Some(Arc::new(publication.identity)),
            Some(publication_coverage),
            Some(publication_attempt),
            retired_snapshot_count,
        )
    } else {
        if load_latest_source_failure_evidence(connection)?.is_some() {
            return Err(corrupt_catalog_state(
                "catalog state without a retained snapshot contains source-failure evidence",
            ));
        }
        if load_integrity_failure_evidence(connection)?.is_some() {
            return Err(corrupt_catalog_state(
                "non-error catalog state contains integrity-failure evidence",
            ));
        }
        (
            None,
            None,
            None,
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
                || expected.predecessor_snapshot.readiness_epoch != expected.epoch
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
                || expected.predecessor_snapshot.readiness_epoch != expected.epoch
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
                || expected.predecessor_snapshot.readiness_epoch != expected.epoch
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
        CatalogBuildStateCommand::RetryDegradedRefresh {
            expected,
            started_at,
            committed_at,
        } => {
            if expected.scope != CatalogCoverageScope::Library
                || expected.desired_contract_version == 0
                || expected.epoch == 0
                || expected.attempt == 0
                || expected.state != CatalogDurableBuildPhase::Degraded
            {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery expectation is outside one degraded Library lineage"
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
                                      'catalog.library.refresh.recovery_started'
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
               OR NOT (
                    (f.attempt = retained.attempt
                     AND refresh.reason IN (
                       'catalog.library.refresh.started',
                       'catalog.library.refresh.source_retrying'
                     ))
                    OR
                    (f.attempt > retained.attempt
                     AND refresh.reason IN (
                       'catalog.library.refresh.recovery_started',
                       'catalog.library.refresh.source_retrying'
                     ))
               )
               OR f.coverage_plan_id != retained.coverage_plan_id
               OR f.readiness_epoch != retained.readiness_epoch
               OR f.retained_publication_digest != retained.publication_digest
               OR f.retained_content_digest != retained.content_digest
               OR NOT (
                    f.retained_snapshot_commit_seq < f.failed_refresh_commit_seq
                    AND f.failed_refresh_commit_seq < f.failure_commit_seq
               )
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
    retained_snapshot_commit_seq: i64,
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

fn load_integrity_failure_evidence(
    connection: &Connection,
) -> Result<Option<StoredCatalogIntegrityFailure>, EngineError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM catalog_refresh_integrity_failures LIMIT 2)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("count catalog integrity failures", error))?;
    if count == 0 {
        return Ok(None);
    }
    if count != 1 {
        return Err(corrupt_catalog_state(
            "catalog integrity-failure evidence exceeds its single active-refresh bound",
        ));
    }
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
                   CASE WHEN typeof(f.snapshot_disposition) = 'text'
                                  AND f.snapshot_disposition = 'independently_safe'
                        THEN f.snapshot_disposition END,
                   f.failed_at,
                   failed.source_instance_id,
                   CASE WHEN typeof(failed.reason) = 'text'
                                  AND failed.reason = 'catalog.library.refresh.integrity_failed'
                        THEN failed.reason END,
                   failed.started_at,
                   failed.committed_at,
                   failed.fact_count,
                   refresh.source_instance_id,
                   CASE WHEN typeof(refresh.reason) = 'text'
                                  AND refresh.reason IN (
                                      'catalog.library.refresh.started',
                                      'catalog.library.refresh.source_retrying'
                                  )
                        THEN refresh.reason END,
                   refresh.committed_at,
                   refresh.fact_count
            FROM catalog_refresh_integrity_failures AS f
            LEFT JOIN ingest_commits AS failed
              ON failed.commit_seq = f.failure_commit_seq
            LEFT JOIN ingest_commits AS refresh
              ON refresh.commit_seq = f.failed_refresh_commit_seq
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

fn exact_integrity_failure_exists(
    connection: &Connection,
    current: &DurableCatalogBuildState,
    expected: &CatalogActiveRefreshPublicationExpectation,
    reason_code: &str,
    started_at: i64,
    committed_at: i64,
) -> Result<bool, EngineError> {
    let Some(stored) = load_integrity_failure_evidence(connection)? else {
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
    Ok(current.readiness.state == CatalogReadinessPhase::Error
        && current.readiness.last_complete_snapshot == Some(expected.predecessor_snapshot)
        && current.readiness.complete_through_commit
            == Some(expected.predecessor_snapshot.complete_commit)
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
            stored.retained_snapshot_commit_seq,
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
        && stored.refresh_reason.as_deref()
            == Some(if expected.retry_reason_code().is_some() {
                REFRESH_SOURCE_RETRYING_REASON
            } else {
                REFRESH_STARTED_REASON
            })
        && stored.refresh_committed_at.is_some()
        && stored.refresh_fact_count == 0)
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
        && stored.refresh_reason.as_deref()
            == Some(if expected.retry_reason_code().is_some() {
                REFRESH_SOURCE_RETRYING_REASON
            } else if expected.is_recovery() {
                REFRESH_RECOVERY_STARTED_REASON
            } else {
                REFRESH_STARTED_REASON
            })
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
    let expected_refresh_reason = if failure_attempt > context.publication_attempt {
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
            != context.retained_snapshot.readiness_epoch
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
                if reason == expected_refresh_reason || reason == REFRESH_SOURCE_RETRYING_REASON
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
    epoch: u64,
    attempt: u64,
    state_commit_seq: u64,
    updated_at: i64,
    retained_snapshot: CatalogSnapshotId,
    publication_identity: &'a CatalogReadyPublicationIdentity,
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
    let stored_snapshot = positive_u64(
        stored.retained_snapshot_commit_seq,
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
    if failure_commit != context.state_commit_seq
        || refresh_commit <= context.retained_snapshot.complete_commit
        || refresh_commit >= failure_commit
        || stored.coverage_plan_id.as_deref()
            != Some(context.plan.coverage_plan_id.storage_bytes().as_slice())
        || positive_u64(stored.readiness_epoch, "catalog failure epoch")? != context.epoch
        || positive_u64(stored.attempt, "catalog failure attempt")? != context.attempt
        || stored_snapshot != context.retained_snapshot.complete_commit
        || retained_commitment.snapshot_id() != context.retained_snapshot
        || stored.retained_publication_digest.as_deref()
            != Some(retained_commitment.publication_digest().as_slice())
        || stored.retained_content_digest.as_deref()
            != Some(retained_commitment.content_digest().as_slice())
        || stored.snapshot_disposition.as_deref() != Some("independently_safe")
        || stored.failed_at != context.updated_at
        || stored.failure_source.is_some()
        || stored.failure_reason.as_deref() != Some(REFRESH_INTEGRITY_FAILURE_REASON)
        || stored.failure_started_at > stored.failed_at
        || stored.failure_committed_at != Some(stored.failed_at)
        || stored.failure_fact_count != 0
        || stored.refresh_source.is_some()
        || !matches!(
            stored.refresh_reason.as_deref(),
            Some(REFRESH_STARTED_REASON | REFRESH_SOURCE_RETRYING_REASON)
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
        CatalogBuildStateWrite::Schedule => transaction
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
                ],
            )
            .map_err(|error| sqlite_error("schedule catalog build state", error))?,
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
                    != Some(expected.predecessor_snapshot.complete_commit)
            {
                return Err(EngineError::InvalidCommit(
                    "catalog integrity-failure write differs from its exact active refresh"
                        .to_string(),
                ));
            }
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
            let expected_state = if expected.is_recovery() {
                BUILDING_STATE
            } else {
                READY_STATE
            };
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
                          AND state = 'building'
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
        CatalogBuildStateWrite::RetryDegradedRefresh => {
            if phase != CatalogDurableBuildPhase::Building
                || snapshot.reason.is_some()
                || snapshot.refreshing_from_snapshot.is_some()
                || snapshot.complete_through_commit.is_some()
                || snapshot.last_complete_snapshot.is_none()
            {
                return Err(EngineError::InvalidCommit(
                    "catalog recovery write differs from degraded readiness".to_string(),
                ));
            }
            let prior_attempt = snapshot.attempt.checked_sub(1).ok_or_else(|| {
                EngineError::InvalidCommit("catalog recovery attempt underflow".to_string())
            })?;
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
                      AND state = 'degraded'
                      AND completed_contract_version = ?6
                      AND (
                        complete_through_commit IS NULL
                        OR complete_through_commit = last_complete_snapshot_commit
                      )
                      AND last_complete_snapshot_commit IS NOT NULL
                      AND refreshing_from_snapshot_commit IS NULL
                      AND reason_code IS NOT NULL
                    "#,
                    params![
                        to_i64(snapshot.attempt, "catalog recovery attempt")?,
                        to_i64(commit_seq, "catalog recovery commit")?,
                        updated_at,
                        LIBRARY_SCOPE,
                        snapshot.coverage_plan_id.storage_bytes().as_slice(),
                        i64::from(snapshot.desired_contract_version),
                        to_i64(snapshot.epoch, "catalog readiness epoch")?,
                        to_i64(prior_attempt, "catalog prior degraded attempt")?,
                    ],
                )
                .map_err(|error| sqlite_error("start degraded catalog recovery", error))?
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
) -> Result<(), EngineError> {
    let (schema_version, payload) = if snapshot.state == CatalogReadinessPhase::Degraded {
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
        let complete_through_commit = snapshot.complete_through_commit.ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog integrity-failure invalidation is missing its complete commit".to_string(),
            )
        })?;
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
            || complete_through_commit != last_complete_snapshot.complete_commit
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
        (CatalogDurableBuildPhase::Ready, false) => Ok(INITIAL_PUBLICATION_REASON),
        (CatalogDurableBuildPhase::Ready, true) => Ok(REFRESH_STARTED_REASON),
        (CatalogDurableBuildPhase::Degraded, false) => Ok(REFRESH_SOURCE_UNAVAILABLE_REASON),
        (CatalogDurableBuildPhase::Error, false) => Ok(REFRESH_INTEGRITY_FAILURE_REASON),
        (
            CatalogDurableBuildPhase::Pending
            | CatalogDurableBuildPhase::Building
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
        CatalogReadinessPhase::Ready => Ok(CatalogDurableBuildPhase::Ready),
        CatalogReadinessPhase::Degraded => Ok(CatalogDurableBuildPhase::Degraded),
        CatalogReadinessPhase::Error => Ok(CatalogDurableBuildPhase::Error),
        CatalogReadinessPhase::Partial => Err(EngineError::InvalidCommit(
            "this catalog persistence slice does not yet support partial readiness".to_string(),
        )),
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
        CanonicalEntityKey, CanonicalSourceInstanceKey, CoverageDeclarationDigest,
        ExternalEntityRef,
    };
    use crate::catalog_contract::{CatalogAccessPolicyDigest, CatalogCoveragePlanSource};
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
