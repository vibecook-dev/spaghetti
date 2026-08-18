//! Durable, crate-private RFC 012B catalog query-retirement authority.
//!
//! Retirement in this module is logical query retirement only. Snapshot
//! headers and frames remain durable restart evidence; physical compaction,
//! automatic retention policy, and public transport are deliberately outside
//! this boundary.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::adapter::ContractVersionSelection;
use crate::catalog_contract::{CatalogCoveragePlanId, CatalogCoverageScope, CatalogSnapshotId};

use super::catalog_publication::{
    CatalogRetainedSnapshotCommitment, MAX_RETAINED_REFRESH_LINEAGE_DEPTH,
};
use super::commit::{self, ChangeEntry};
use super::{catalog_state, EngineError};

pub(super) const RETIREMENT_REASON: &str = "catalog.library.snapshot.retired";
const RETIREMENT_TOPIC: &str = "catalog.snapshot.retired";
const RETIREMENT_CHANGE_SCHEMA_VERSION: u32 = 1;
const RETIREMENT_CONTRACT_VERSION: u32 = 1;
const DIGEST_BYTES: usize = 32;

/// Exact restart-authenticated authority for retiring the oldest currently
/// query-retained predecessor of one plain Ready Library snapshot.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogSnapshotRetirementExpectation {
    scope: CatalogCoverageScope,
    coverage_plan_id: CatalogCoveragePlanId,
    contract_selection: ContractVersionSelection,
    epoch: u64,
    attempt: u64,
    state_commit_seq: u64,
    retired_prefix_len: usize,
    target: CatalogRetainedSnapshotCommitment,
    successor: CatalogRetainedSnapshotCommitment,
}

impl std::fmt::Debug for CatalogSnapshotRetirementExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogSnapshotRetirementExpectation")
            .field("scope", &self.scope)
            .field("coverage_plan_id", &self.coverage_plan_id)
            .field("epoch", &self.epoch)
            .field("attempt", &self.attempt)
            .field("state_commit_seq", &self.state_commit_seq)
            .field("retired_prefix_len", &self.retired_prefix_len)
            .field("target_snapshot", &self.target.snapshot_id())
            .field("successor_snapshot", &self.successor.snapshot_id())
            .finish_non_exhaustive()
    }
}

impl CatalogSnapshotRetirementExpectation {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        scope: CatalogCoverageScope,
        coverage_plan_id: CatalogCoveragePlanId,
        contract_selection: ContractVersionSelection,
        epoch: u64,
        attempt: u64,
        state_commit_seq: u64,
        retired_prefix_len: usize,
        target: CatalogRetainedSnapshotCommitment,
        successor: CatalogRetainedSnapshotCommitment,
    ) -> Result<Self, EngineError> {
        let value = Self {
            scope,
            coverage_plan_id,
            contract_selection,
            epoch,
            attempt,
            state_commit_seq,
            retired_prefix_len,
            target,
            successor,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), EngineError> {
        let target = self.target.snapshot_id();
        let successor = self.successor.snapshot_id();
        if self.scope != CatalogCoverageScope::Library
            || self.epoch == 0
            || self.attempt == 0
            || self.state_commit_seq == 0
            || self.retired_prefix_len >= MAX_RETAINED_REFRESH_LINEAGE_DEPTH
            || target.coverage_plan_id != self.coverage_plan_id
            || successor.coverage_plan_id != self.coverage_plan_id
            || target.pack_contract_version != successor.pack_contract_version
            || target.readiness_epoch != self.epoch
            || successor.readiness_epoch != self.epoch
            || target.complete_commit >= successor.complete_commit
            || successor.complete_commit != self.state_commit_seq
            || self.contract_selection.query_pack_version != Some(successor.pack_contract_version)
            || self
                .target
                .publication_digest()
                .iter()
                .all(|byte| *byte == 0)
            || self.target.content_digest().iter().all(|byte| *byte == 0)
            || self
                .successor
                .publication_digest()
                .iter()
                .all(|byte| *byte == 0)
            || self
                .successor
                .content_digest()
                .iter()
                .all(|byte| *byte == 0)
        {
            return Err(EngineError::InvalidCommit(
                "catalog retirement expectation is outside one exact plain Ready lineage"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn target(&self) -> CatalogRetainedSnapshotCommitment {
        self.target
    }

    pub(super) fn successor(&self) -> CatalogRetainedSnapshotCommitment {
        self.successor
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogSnapshotRetirementCommand {
    expected: CatalogSnapshotRetirementExpectation,
    started_at: i64,
    committed_at: i64,
}

impl std::fmt::Debug for CatalogSnapshotRetirementCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogSnapshotRetirementCommand")
            .field("expected", &self.expected)
            .field("started_at", &self.started_at)
            .field("committed_at", &self.committed_at)
            .finish_non_exhaustive()
    }
}

impl CatalogSnapshotRetirementCommand {
    pub(crate) fn new(
        expected: CatalogSnapshotRetirementExpectation,
        started_at: i64,
        committed_at: i64,
    ) -> Result<Self, EngineError> {
        expected.validate()?;
        if committed_at < started_at {
            return Err(EngineError::InvalidCommit(
                "catalog retirement commit time must not precede its start".to_string(),
            ));
        }
        Ok(Self {
            expected,
            started_at,
            committed_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogSnapshotRetirementReceipt {
    pub commit_seq: u64,
    pub retired_snapshot: CatalogSnapshotId,
    pub latest_snapshot: CatalogSnapshotId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogRetirementCommitStage {
    BeforeTransaction,
    AfterCommitInsert,
    AfterRetirementInsert,
    AfterOutboxInsert,
    BeforeCommit,
    AfterCommit,
}

pub(super) trait CatalogRetirementCommitHook {
    fn reach(&self, stage: CatalogRetirementCommitStage) -> Result<(), EngineError>;
}

struct NoopCatalogRetirementCommitHook;

impl CatalogRetirementCommitHook for NoopCatalogRetirementCommitHook {
    fn reach(&self, _stage: CatalogRetirementCommitStage) -> Result<(), EngineError> {
        Ok(())
    }
}

pub(super) fn apply_catalog_snapshot_retirement(
    connection: &mut Connection,
    command: &CatalogSnapshotRetirementCommand,
) -> Result<Option<CatalogSnapshotRetirementReceipt>, EngineError> {
    apply_catalog_snapshot_retirement_with_hook(
        connection,
        command,
        &NoopCatalogRetirementCommitHook,
    )
}

pub(super) fn apply_catalog_snapshot_retirement_with_hook(
    connection: &mut Connection,
    command: &CatalogSnapshotRetirementCommand,
    hook: &dyn CatalogRetirementCommitHook,
) -> Result<Option<CatalogSnapshotRetirementReceipt>, EngineError> {
    command.expected.validate()?;
    if command.committed_at < command.started_at {
        return Err(EngineError::InvalidCommit(
            "catalog retirement commit time must not precede its start".to_string(),
        ));
    }

    hook.reach(CatalogRetirementCommitStage::BeforeTransaction)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| catalog_state::sqlite_error("begin catalog snapshot retirement", error))?;
    let current = catalog_state::load_catalog_build_state(&transaction)?.ok_or_else(|| {
        EngineError::InvalidCommit(
            "catalog snapshot retirement requires a durable Ready lineage".to_string(),
        )
    })?;

    if exact_retirement_exists(&transaction, &command.expected)? {
        transaction.commit().map_err(|error| {
            catalog_state::sqlite_error("finish unchanged catalog snapshot retirement", error)
        })?;
        return Ok(None);
    }

    let actual = current.snapshot_retirement_expectation()?;
    if actual != command.expected {
        return Err(EngineError::InvalidCommit(
            "catalog snapshot retirement differs from the exact current oldest-retained expectation"
                .to_string(),
        ));
    }

    let commit_seq = catalog_state::insert_administrative_commit(
        &transaction,
        RETIREMENT_REASON,
        command.started_at,
        command.committed_at,
    )?;
    hook.reach(CatalogRetirementCommitStage::AfterCommitInsert)?;

    let target = command.expected.target();
    let successor = command.expected.successor();
    transaction
        .execute(
            r#"
            INSERT INTO catalog_snapshot_retirements (
                snapshot_commit_seq, snapshot_publication_digest,
                snapshot_content_digest, successor_snapshot_commit_seq,
                successor_publication_digest, successor_content_digest,
                retirement_commit_seq, retired_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                catalog_state::to_i64(
                    target.snapshot_id().complete_commit,
                    "retired catalog snapshot commit",
                )?,
                target.publication_digest().as_slice(),
                target.content_digest().as_slice(),
                catalog_state::to_i64(
                    successor.snapshot_id().complete_commit,
                    "catalog retirement successor commit",
                )?,
                successor.publication_digest().as_slice(),
                successor.content_digest().as_slice(),
                catalog_state::to_i64(commit_seq, "catalog retirement commit")?,
                command.committed_at,
            ],
        )
        .map_err(|error| {
            catalog_state::sqlite_error("insert catalog retirement evidence", error)
        })?;
    hook.reach(CatalogRetirementCommitStage::AfterRetirementInsert)?;

    write_retirement_change(
        &transaction,
        commit_seq,
        target.snapshot_id(),
        successor.snapshot_id(),
    )?;
    hook.reach(CatalogRetirementCommitStage::AfterOutboxInsert)?;
    hook.reach(CatalogRetirementCommitStage::BeforeCommit)?;
    transaction.commit().map_err(|error| {
        catalog_state::sqlite_error("commit catalog snapshot retirement", error)
    })?;
    hook.reach(CatalogRetirementCommitStage::AfterCommit)?;

    Ok(Some(CatalogSnapshotRetirementReceipt {
        commit_seq,
        retired_snapshot: target.snapshot_id(),
        latest_snapshot: successor.snapshot_id(),
    }))
}

fn exact_retirement_exists(
    connection: &Connection,
    expected: &CatalogSnapshotRetirementExpectation,
) -> Result<bool, EngineError> {
    let target = expected.target();
    let successor = expected.successor();
    let stored = connection
        .query_row(
            r#"
            SELECT CASE WHEN typeof(snapshot_publication_digest) = 'blob'
                                      AND length(snapshot_publication_digest) = 32
                               THEN snapshot_publication_digest END,
                   CASE WHEN typeof(snapshot_content_digest) = 'blob'
                                      AND length(snapshot_content_digest) = 32
                               THEN snapshot_content_digest END,
                   successor_snapshot_commit_seq,
                   CASE WHEN typeof(successor_publication_digest) = 'blob'
                                      AND length(successor_publication_digest) = 32
                               THEN successor_publication_digest END,
                   CASE WHEN typeof(successor_content_digest) = 'blob'
                                      AND length(successor_content_digest) = 32
                               THEN successor_content_digest END
            FROM catalog_snapshot_retirements
            WHERE snapshot_commit_seq = ?1
            "#,
            [catalog_state::to_i64(
                target.snapshot_id().complete_commit,
                "retired catalog snapshot commit",
            )?],
            |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| catalog_state::sqlite_error("load catalog retirement replay", error))?;
    let Some((
        target_publication,
        target_content,
        successor_commit,
        successor_publication,
        successor_content,
    )) = stored
    else {
        return Ok(false);
    };
    let exact = target_publication.as_deref() == Some(target.publication_digest().as_slice())
        && target_content.as_deref() == Some(target.content_digest().as_slice())
        && catalog_state::positive_u64(successor_commit, "catalog retirement successor")?
            == successor.snapshot_id().complete_commit
        && successor_publication.as_deref() == Some(successor.publication_digest().as_slice())
        && successor_content.as_deref() == Some(successor.content_digest().as_slice());
    if !exact {
        return Err(EngineError::InvalidCommit(
            "catalog snapshot retirement conflicts with existing durable evidence".to_string(),
        ));
    }
    Ok(true)
}

/// Validate the complete append-only retirement prefix against the exact
/// restart-authenticated snapshot chain. The query stops one row beyond the
/// maximum honest prefix so corrupt databases cannot cause unbounded work.
pub(super) fn load_retired_prefix(
    connection: &Connection,
    chain: &[CatalogRetainedSnapshotCommitment],
) -> Result<usize, EngineError> {
    let scan_limit = MAX_RETAINED_REFRESH_LINEAGE_DEPTH
        .checked_add(1)
        .ok_or_else(|| catalog_state::corrupt_catalog_state("retirement scan limit overflow"))?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT r.snapshot_commit_seq,
                   CASE WHEN typeof(r.snapshot_publication_digest) = 'blob'
                                      AND length(r.snapshot_publication_digest) = 32
                               THEN r.snapshot_publication_digest END,
                   CASE WHEN typeof(r.snapshot_content_digest) = 'blob'
                                      AND length(r.snapshot_content_digest) = 32
                               THEN r.snapshot_content_digest END,
                   r.successor_snapshot_commit_seq,
                   CASE WHEN typeof(r.successor_publication_digest) = 'blob'
                                      AND length(r.successor_publication_digest) = 32
                               THEN r.successor_publication_digest END,
                   CASE WHEN typeof(r.successor_content_digest) = 'blob'
                                      AND length(r.successor_content_digest) = 32
                               THEN r.successor_content_digest END,
                   r.retirement_commit_seq, r.retired_at,
                   CASE WHEN c.commit_seq IS NOT NULL THEN 1 ELSE 0 END,
                   c.source_instance_id,
                   CASE WHEN typeof(c.reason) = 'text'
                                      AND length(CAST(c.reason AS BLOB)) BETWEEN 1 AND 128
                               THEN c.reason END,
                   c.committed_at, c.fact_count
            FROM catalog_snapshot_retirements AS r
            LEFT JOIN ingest_commits AS c ON c.commit_seq = r.retirement_commit_seq
            ORDER BY r.snapshot_commit_seq
            LIMIT ?1
            "#,
        )
        .map_err(|error| catalog_state::sqlite_error("prepare catalog retirement prefix", error))?;
    let mut rows = statement
        .query([catalog_state::to_i64(
            scan_limit as u64,
            "catalog retirement scan limit",
        )?])
        .map_err(|error| catalog_state::sqlite_error("query catalog retirement prefix", error))?;
    let mut retired = 0_usize;
    let mut previous_retirement_commit = None;
    while let Some(row) = rows
        .next()
        .map_err(|error| catalog_state::sqlite_error("read catalog retirement prefix", error))?
    {
        if retired >= chain.len().saturating_sub(1) || retired >= MAX_RETAINED_REFRESH_LINEAGE_DEPTH
        {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog retirement evidence exceeds the bounded non-current ancestry",
            ));
        }
        let target_commit = catalog_state::positive_u64(
            row.get(0).map_err(|error| {
                catalog_state::sqlite_error("decode retired catalog snapshot", error)
            })?,
            "retired catalog snapshot commit",
        )?;
        let target = chain[retired];
        let target_publication = decode_digest(
            row.get::<_, Option<Vec<u8>>>(1)
                .map_err(|error| {
                    catalog_state::sqlite_error("decode retired publication digest", error)
                })?
                .as_deref(),
            "retired catalog publication digest",
        )?;
        let target_content = decode_digest(
            row.get::<_, Option<Vec<u8>>>(2)
                .map_err(|error| {
                    catalog_state::sqlite_error("decode retired content digest", error)
                })?
                .as_deref(),
            "retired catalog content digest",
        )?;
        if target_commit != target.snapshot_id().complete_commit
            || target_publication != target.publication_digest()
            || target_content != target.content_digest()
        {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog retirement rows are not the exact canonical oldest ancestry prefix",
            ));
        }

        let successor_commit = catalog_state::positive_u64(
            row.get(3).map_err(|error| {
                catalog_state::sqlite_error("decode catalog retirement successor", error)
            })?,
            "catalog retirement successor commit",
        )?;
        let successor = chain
            .iter()
            .copied()
            .find(|candidate| candidate.snapshot_id().complete_commit == successor_commit)
            .ok_or_else(|| {
                catalog_state::corrupt_catalog_state(
                    "catalog retirement successor is outside the current ancestry",
                )
            })?;
        if successor_commit <= target_commit
            || decode_digest(
                row.get::<_, Option<Vec<u8>>>(4)
                    .map_err(|error| {
                        catalog_state::sqlite_error(
                            "decode catalog retirement successor publication",
                            error,
                        )
                    })?
                    .as_deref(),
                "catalog retirement successor publication digest",
            )? != successor.publication_digest()
            || decode_digest(
                row.get::<_, Option<Vec<u8>>>(5)
                    .map_err(|error| {
                        catalog_state::sqlite_error(
                            "decode catalog retirement successor content",
                            error,
                        )
                    })?
                    .as_deref(),
                "catalog retirement successor content digest",
            )? != successor.content_digest()
        {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog retirement successor commitment is invalid",
            ));
        }

        let retirement_commit = catalog_state::positive_u64(
            row.get(6).map_err(|error| {
                catalog_state::sqlite_error("decode catalog retirement commit", error)
            })?,
            "catalog retirement commit",
        )?;
        let exact_successor_at_retirement = chain
            .iter()
            .copied()
            .take_while(|candidate| candidate.snapshot_id().complete_commit < retirement_commit)
            .last();
        if exact_successor_at_retirement != Some(successor) {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog retirement successor is not the exact current snapshot at its commit",
            ));
        }
        let retired_at: i64 = row.get(7).map_err(|error| {
            catalog_state::sqlite_error("decode catalog retirement time", error)
        })?;
        let owner_exists: i64 = row.get(8).map_err(|error| {
            catalog_state::sqlite_error("decode catalog retirement owner", error)
        })?;
        let owner_source: Option<i64> = row.get(9).map_err(|error| {
            catalog_state::sqlite_error("decode catalog retirement owner source", error)
        })?;
        let owner_reason: Option<String> = row.get(10).map_err(|error| {
            catalog_state::sqlite_error("decode catalog retirement owner reason", error)
        })?;
        let owner_committed_at: Option<i64> = row.get(11).map_err(|error| {
            catalog_state::sqlite_error("decode catalog retirement owner time", error)
        })?;
        let owner_fact_count: Option<i64> = row.get(12).map_err(|error| {
            catalog_state::sqlite_error("decode catalog retirement owner fact count", error)
        })?;
        if owner_exists != 1
            || retirement_commit <= successor_commit
            || previous_retirement_commit.is_some_and(|previous| previous >= retirement_commit)
            || owner_committed_at != Some(retired_at)
        {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog retirement commit lineage is invalid",
            ));
        }
        catalog_state::validate_admin_commit(
            owner_source,
            owner_reason.as_deref().unwrap_or_default(),
            owner_committed_at,
            owner_fact_count.unwrap_or(-1),
            RETIREMENT_REASON,
        )?;
        previous_retirement_commit = Some(retirement_commit);
        retired += 1;
    }
    Ok(retired)
}

pub(super) fn ensure_snapshot_query_retained(
    connection: &Connection,
    snapshot_id: CatalogSnapshotId,
) -> Result<(), EngineError> {
    let retired = connection
        .query_row(
            "SELECT 1 FROM catalog_snapshot_retirements WHERE snapshot_commit_seq = ?1",
            [catalog_state::to_i64(
                snapshot_id.complete_commit,
                "catalog query snapshot commit",
            )?],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| catalog_state::sqlite_error("check catalog query retirement", error))?;
    if retired.is_some() {
        return Err(EngineError::InvalidCommit(
            "catalog snapshot is durably retired from query service".to_string(),
        ));
    }
    Ok(())
}

fn decode_digest(value: Option<&[u8]>, label: &str) -> Result<[u8; DIGEST_BYTES], EngineError> {
    let value = value.ok_or_else(|| {
        catalog_state::corrupt_catalog_state(format!("{label} exceeds its fixed durable bound"))
    })?;
    value.try_into().map_err(|_| {
        catalog_state::corrupt_catalog_state(format!("{label} must contain exactly 32 bytes"))
    })
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogSnapshotRetiredPayload {
    retirement_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: String,
    readiness_epoch: u64,
    retired_snapshot: CatalogSnapshotId,
    latest_snapshot: CatalogSnapshotId,
    commit_seq: u64,
}

fn write_retirement_change(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    retired_snapshot: CatalogSnapshotId,
    latest_snapshot: CatalogSnapshotId,
) -> Result<(), EngineError> {
    let payload = serde_json::to_vec(&CatalogSnapshotRetiredPayload {
        retirement_contract_version: RETIREMENT_CONTRACT_VERSION,
        scope: catalog_state::LIBRARY_SCOPE,
        coverage_plan_id: format!(
            "v1:{}",
            URL_SAFE_NO_PAD.encode(retired_snapshot.coverage_plan_id.storage_bytes())
        ),
        readiness_epoch: retired_snapshot.readiness_epoch,
        retired_snapshot,
        latest_snapshot,
        commit_seq,
    })
    .map_err(|error| {
        EngineError::InvalidCommit(format!(
            "could not encode catalog snapshot-retirement invalidation: {error}"
        ))
    })?;
    commit::write_internal_changes(
        transaction,
        commit_seq,
        &[ChangeEntry {
            topic: RETIREMENT_TOPIC.to_string(),
            schema_version: RETIREMENT_CHANGE_SCHEMA_VERSION,
            entity_key: retired_snapshot.coverage_plan_id.storage_bytes().to_vec(),
            operation: "upsert".to_string(),
            payload,
        }],
    )
}

#[cfg(test)]
mod tests;
