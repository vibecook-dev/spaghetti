//! Source-neutral RFC 012B Library plan registration and initial build state.
//!
//! This module deliberately stops at `Pending`/`Building`. It persists no
//! source coverage, reduced rows, completed snapshot, or query authority. A
//! later B3 transition must compose those values atomically before readiness
//! may advance beyond this administrative lineage.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::catalog_contract::{
    CatalogCoveragePlan, CatalogCoveragePlanId, CatalogCoverageScope, CatalogReadinessMachine,
    CatalogReadinessPhase, CatalogReadinessSnapshot, CATALOG_READINESS_CONTRACT_VERSION,
};

use super::commit::{self, ChangeEntry};
use super::EngineError;

const LIBRARY_SCOPE: &str = "library";
const PENDING_STATE: &str = "pending";
const BUILDING_STATE: &str = "building";
const REGISTER_REASON: &str = "catalog.library.plan.registered";
const SCHEDULE_REASON: &str = "catalog.library.build.scheduled";
const READINESS_CHANGE_TOPIC: &str = "catalog.readiness.changed";
const READINESS_CHANGE_SCHEMA_VERSION: u32 = 1;
const MAX_CATALOG_PLAN_JSON_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogDurableBuildPhase {
    Pending,
    Building,
}

impl CatalogDurableBuildPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => PENDING_STATE,
            Self::Building => BUILDING_STATE,
        }
    }

    fn parse(value: &str) -> Result<Self, EngineError> {
        match value {
            PENDING_STATE => Ok(Self::Pending),
            BUILDING_STATE => Ok(Self::Building),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableCatalogBuildState {
    pub plan: CatalogCoveragePlan,
    pub readiness: CatalogReadinessSnapshot,
    pub last_commit_seq: u64,
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

    let (mut machine, started_at, committed_at, reason, write_plan) = match command {
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
                        CatalogReadinessPhase::Pending | CatalogReadinessPhase::Building
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
                true,
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
                    ..actual
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
            (machine, *started_at, *committed_at, SCHEDULE_REASON, false)
        }
    };

    let commit_seq = insert_administrative_commit(&transaction, reason, started_at, committed_at)?;
    hook.reach(CatalogCommitStage::AfterCommitInsert)?;
    if write_plan {
        insert_plan(&transaction, machine.plan(), commit_seq)?;
    }
    hook.reach(CatalogCommitStage::AfterPlanWrite)?;
    write_build_state(
        &transaction,
        &mut machine,
        commit_seq,
        committed_at,
        write_plan,
    )?;
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
            SELECT plan.coverage_plan_id,
                   plan.coverage_plan_contract_version,
                   CASE
                     WHEN length(plan.plan_json) BETWEEN 1 AND ?2
                     THEN plan.plan_json
                   END,
                   plan.content_digest,
                   plan.created_commit_seq,
                   build.desired_contract_version,
                   build.epoch,
                   build.attempt,
                   build.state,
                   build.last_commit_seq,
                   build.updated_at,
                   plan_commit.source_instance_id,
                   plan_commit.reason,
                   plan_commit.committed_at,
                   plan_commit.fact_count,
                   state_commit.source_instance_id,
                   state_commit.reason,
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
                    last_commit_seq: row.get(9)?,
                    updated_at: row.get(10)?,
                    plan_commit_source: row.get(11)?,
                    plan_commit_reason: row.get(12)?,
                    plan_committed_at: row.get(13)?,
                    plan_fact_count: row.get(14)?,
                    state_commit_source: row.get(15)?,
                    state_commit_reason: row.get(16)?,
                    state_committed_at: row.get(17)?,
                    state_fact_count: row.get(18)?,
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
        Some(row) if plan_count == 1 && state_count == 1 => decode_stored_state(row).map(Some),
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
    coverage_plan_id: Vec<u8>,
    coverage_plan_contract_version: i64,
    plan_json: Option<Vec<u8>>,
    content_digest: Vec<u8>,
    created_commit_seq: i64,
    desired_contract_version: i64,
    epoch: i64,
    attempt: i64,
    state: String,
    last_commit_seq: i64,
    updated_at: i64,
    plan_commit_source: Option<i64>,
    plan_commit_reason: String,
    plan_committed_at: Option<i64>,
    plan_fact_count: i64,
    state_commit_source: Option<i64>,
    state_commit_reason: String,
    state_committed_at: Option<i64>,
    state_fact_count: i64,
}

fn decode_stored_state(
    stored: StoredCatalogBuildState,
) -> Result<DurableCatalogBuildState, EngineError> {
    let Some(plan_json) = stored.plan_json else {
        return Err(corrupt_catalog_state(
            "catalog coverage-plan JSON is outside its durable byte bound",
        ));
    };
    if stored.coverage_plan_id.len() != 32 || stored.content_digest.len() != 32 {
        return Err(corrupt_catalog_state(
            "catalog plan identity or content digest is not 32 bytes",
        ));
    }
    if blake3::hash(&plan_json).as_bytes() != stored.content_digest.as_slice() {
        return Err(corrupt_catalog_state(
            "catalog coverage-plan content digest does not match stored bytes",
        ));
    }
    let plan: CatalogCoveragePlan = serde_json::from_slice(&plan_json).map_err(|error| {
        corrupt_catalog_state(format!("catalog coverage-plan JSON is invalid: {error}"))
    })?;
    plan.validate().map_err(catalog_contract_error)?;
    if plan.scope != CatalogCoverageScope::Library
        || plan.coverage_plan_id.storage_bytes().as_slice() != stored.coverage_plan_id.as_slice()
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
    let phase = CatalogDurableBuildPhase::parse(&stored.state)?;
    let created_commit_seq = positive_u64(stored.created_commit_seq, "catalog plan commit")?;
    let last_commit_seq = positive_u64(stored.last_commit_seq, "catalog state commit")?;
    validate_admin_commit(
        stored.plan_commit_source,
        &stored.plan_commit_reason,
        stored.plan_committed_at,
        stored.plan_fact_count,
        REGISTER_REASON,
    )?;
    validate_admin_commit(
        stored.state_commit_source,
        &stored.state_commit_reason,
        stored.state_committed_at,
        stored.state_fact_count,
        phase_reason(phase),
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
        _ => {}
    }

    let mut machine = CatalogReadinessMachine::register(plan.clone(), desired_contract_version)
        .map_err(catalog_contract_error)?;
    if phase == CatalogDurableBuildPhase::Building {
        machine.schedule_build().map_err(catalog_contract_error)?;
    }
    if machine.snapshot().epoch != epoch || machine.snapshot().attempt != attempt {
        return Err(corrupt_catalog_state(
            "catalog build epoch or attempt cannot be reconstructed from the persisted lineage",
        ));
    }
    Ok(DurableCatalogBuildState {
        plan,
        readiness: machine.snapshot().clone(),
        last_commit_seq,
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
    };
    if committed_at < started_at {
        return Err(EngineError::InvalidCommit(
            "catalog build-state commit time must not precede its start".to_string(),
        ));
    }
    Ok(())
}

fn insert_administrative_commit(
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

fn write_build_state(
    transaction: &Transaction<'_>,
    machine: &mut CatalogReadinessMachine,
    commit_seq: u64,
    updated_at: i64,
    inserting: bool,
) -> Result<(), EngineError> {
    let snapshot = machine.snapshot();
    let phase = durable_phase(snapshot.state)?;
    let changed = if inserting {
        transaction
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
            .map_err(|error| sqlite_error("insert catalog build state", error))?
    } else {
        transaction
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
            .map_err(|error| sqlite_error("advance catalog build state", error))?
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
    let payload = serde_json::to_vec(&CatalogReadinessChangedPayload {
        readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
        scope: LIBRARY_SCOPE,
        coverage_plan_id: snapshot.coverage_plan_id,
        desired_contract_version: snapshot.desired_contract_version,
        epoch: snapshot.epoch,
        attempt: snapshot.attempt,
        state: snapshot.state,
        commit_seq,
    })
    .map_err(|error| {
        EngineError::InvalidCommit(format!(
            "could not encode catalog readiness change: {error}"
        ))
    })?;
    commit::write_internal_changes(
        transaction,
        commit_seq,
        &[ChangeEntry {
            topic: READINESS_CHANGE_TOPIC.to_string(),
            schema_version: READINESS_CHANGE_SCHEMA_VERSION,
            entity_key: snapshot.coverage_plan_id.storage_bytes().to_vec(),
            operation: "upsert".to_string(),
            payload,
        }],
    )
}

fn validate_admin_commit(
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

fn phase_reason(phase: CatalogDurableBuildPhase) -> &'static str {
    match phase {
        CatalogDurableBuildPhase::Pending => REGISTER_REASON,
        CatalogDurableBuildPhase::Building => SCHEDULE_REASON,
    }
}

fn durable_phase(state: CatalogReadinessPhase) -> Result<CatalogDurableBuildPhase, EngineError> {
    match state {
        CatalogReadinessPhase::Pending => Ok(CatalogDurableBuildPhase::Pending),
        CatalogReadinessPhase::Building => Ok(CatalogDurableBuildPhase::Building),
        CatalogReadinessPhase::Partial
        | CatalogReadinessPhase::Ready
        | CatalogReadinessPhase::Degraded
        | CatalogReadinessPhase::Error => Err(EngineError::InvalidCommit(
            "this catalog persistence slice cannot publish coverage-bearing readiness".to_string(),
        )),
    }
}

fn positive_u32(value: i64, field: &'static str) -> Result<u32, EngineError> {
    let value = u32::try_from(value)
        .map_err(|_| corrupt_catalog_state(format!("{field} is outside the durable u32 range")))?;
    if value == 0 {
        return Err(corrupt_catalog_state(format!("{field} must be positive")));
    }
    Ok(value)
}

fn positive_u64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    let value =
        u64::try_from(value).map_err(|_| corrupt_catalog_state(format!("{field} is negative")))?;
    if value == 0 {
        return Err(corrupt_catalog_state(format!("{field} must be positive")));
    }
    Ok(value)
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} exceeds SQLite integer range")))
}

fn catalog_contract_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::InvalidCommit(format!("invalid catalog contract: {error}"))
}

fn corrupt_catalog_state(message: impl Into<String>) -> EngineError {
    EngineError::InvalidCommit(format!(
        "corrupt durable catalog build state: {}",
        message.into()
    ))
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
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
