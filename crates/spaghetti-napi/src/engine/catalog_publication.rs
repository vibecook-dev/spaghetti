//! Atomic durable RFC 012B initial and ordinary-refresh Library publication.
//!
//! This module consumes only checked private B3 publication envelopes. It owns
//! no source reads, retirement/expiration, query execution, or N-API surface.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::adapter::{ContractVersionSelection, SourceCoverageSet};
use crate::catalog_contract::evidence::{
    decode_durable_project_row, decode_durable_reducer_state, decode_durable_session_row,
    decode_durable_tombstone, CatalogEntityKind, CatalogReducer, CatalogReducerPublication,
    CatalogReducerPublicationLimits, CatalogReducerPublicationRevision, CatalogResolutionIndex,
};
use crate::catalog_contract::publication::{
    decode_durable_member_binding_frame, decode_durable_member_history_frame,
    decode_durable_source_frame, derive_durable_content_digest, derive_durable_entries_digest,
    derive_durable_refresh_content_digest, validate_durable_contract_selection,
    validate_restarted_initial_publication, validate_restarted_refresh_publication,
    CatalogDurableInitialPublication, CatalogDurablePublicationEntryKind,
    CatalogDurableRefreshPublication, CatalogDurableSourceFrame, CatalogInitialBuildExpectation,
    CatalogInitialPublicationAssembly, CatalogInitialPublicationDigest,
    CatalogPublicationMemberBinding, CatalogPublicationMemberHistory,
    CatalogRefreshBuildExpectation, CatalogRefreshPredecessor, CatalogRefreshPublicationAssembly,
    CatalogRefreshPublicationDigest, CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION,
    CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION, MAX_DURABLE_CATALOG_ROW_BYTES,
    MAX_DURABLE_PUBLICATION_BYTES, MAX_DURABLE_PUBLICATION_ENTRIES,
};
use crate::catalog_contract::{
    CatalogCoveragePlan, CatalogCoverageScope, CatalogReadinessMachine, CatalogReadinessPhase,
    CatalogReadinessSnapshot, CatalogSnapshotId,
};

use super::catalog_state::CatalogActiveRefreshPublicationExpectation;
use super::catalog_state::{self, DurableCatalogBuildState};
use super::commit::{self, ChangeEntry};
use super::EngineError;

const READINESS_CHANGE_TOPIC: &str = "catalog.readiness.changed";
const READY_CHANGE_SCHEMA_VERSION: u32 = 2;
const REFRESH_READY_CHANGE_SCHEMA_VERSION: u32 = 4;
const DIGEST_BYTES: usize = 32;
/// Until a retirement policy exists, bound both restart work and the number
/// of immutable retained predecessors one Ready authority may depend on.
pub(super) const MAX_RETAINED_REFRESH_LINEAGE_DEPTH: usize = 8;

#[derive(Clone)]
pub(crate) struct CatalogInitialPublicationCommand {
    assembly: CatalogInitialPublicationAssembly,
    expected_build_commit_seq: u64,
    started_at: i64,
    committed_at: i64,
}

impl CatalogInitialPublicationCommand {
    pub(crate) fn new(
        assembly: CatalogInitialPublicationAssembly,
        expected_build_commit_seq: u64,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self {
            assembly,
            expected_build_commit_seq,
            started_at,
            committed_at,
        }
    }
}

impl std::fmt::Debug for CatalogInitialPublicationCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogInitialPublicationCommand")
            .field("assembly", &self.assembly)
            .field("expected_build_commit_seq", &self.expected_build_commit_seq)
            .field("started_at", &self.started_at)
            .field("committed_at", &self.committed_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogInitialPublicationReceipt {
    pub commit_seq: u64,
    pub snapshot_id: CatalogSnapshotId,
    pub readiness: CatalogReadinessSnapshot,
}

#[derive(Clone)]
pub(crate) struct CatalogRefreshPublicationCommand {
    assembly: CatalogRefreshPublicationAssembly,
    expected: CatalogActiveRefreshPublicationExpectation,
    started_at: i64,
    committed_at: i64,
}

impl CatalogRefreshPublicationCommand {
    pub(crate) fn new(
        assembly: CatalogRefreshPublicationAssembly,
        expected: CatalogActiveRefreshPublicationExpectation,
        started_at: i64,
        committed_at: i64,
    ) -> Self {
        Self {
            assembly,
            expected,
            started_at,
            committed_at,
        }
    }
}

impl std::fmt::Debug for CatalogRefreshPublicationCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogRefreshPublicationCommand")
            .field("assembly", &self.assembly)
            .field("expected", &self.expected)
            .field("started_at", &self.started_at)
            .field("committed_at", &self.committed_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogRefreshPublicationReceipt {
    pub commit_seq: u64,
    pub predecessor_snapshot: CatalogSnapshotId,
    pub snapshot_id: CatalogSnapshotId,
    pub readiness: CatalogReadinessSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogPublicationCommitStage {
    BeforeTransaction,
    AfterCommitInsert,
    AfterSnapshotWrite,
    AfterSourceEntries,
    AfterEvidenceEntries,
    AfterProjectRows,
    AfterSessionRows,
    AfterTombstones,
    AfterReadinessWrite,
    AfterOutboxInsert,
    BeforeCommit,
    AfterCommit,
}

pub(super) trait CatalogPublicationCommitHook {
    fn reach(&self, stage: CatalogPublicationCommitStage) -> Result<(), EngineError>;
}

struct NoopCatalogPublicationCommitHook;

impl CatalogPublicationCommitHook for NoopCatalogPublicationCommitHook {
    fn reach(&self, _stage: CatalogPublicationCommitStage) -> Result<(), EngineError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CatalogSnapshotLineage {
    Initial,
    Refresh {
        predecessor_snapshot: CatalogSnapshotId,
        predecessor_publication_digest: [u8; DIGEST_BYTES],
        predecessor_content_digest: [u8; DIGEST_BYTES],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogReadyPublicationHeaderIdentity {
    build_commit_seq: u64,
    durable_contract_version: u32,
    lineage: CatalogSnapshotLineage,
    contract_selection: ContractVersionSelection,
    member_identity_contract_id: Option<String>,
    publication_digest: [u8; DIGEST_BYTES],
    reducer_revision: CatalogReducerPublicationRevision,
    entries_digest: [u8; DIGEST_BYTES],
    content_digest: [u8; DIGEST_BYTES],
    entry_count: usize,
    encoded_bytes: usize,
    source_count: usize,
    member_count: usize,
    project_row_count: usize,
    session_row_count: usize,
    tombstone_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogReadyRowCommitment {
    key: [u8; DIGEST_BYTES],
    payload_digest: [u8; DIGEST_BYTES],
    payload_len: u32,
}

/// Bounded restart-authenticated identity for one snapshot in the current
/// linear refresh ancestry. It is intentionally small: historical row and
/// reducer commitments are loaded on demand and must match this frozen proof.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct CatalogRetainedSnapshotCommitment {
    snapshot_id: CatalogSnapshotId,
    publication_digest: [u8; DIGEST_BYTES],
    content_digest: [u8; DIGEST_BYTES],
}

impl std::fmt::Debug for CatalogRetainedSnapshotCommitment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogRetainedSnapshotCommitment")
            .field("snapshot_id", &self.snapshot_id)
            .finish_non_exhaustive()
    }
}

impl CatalogRetainedSnapshotCommitment {
    #[cfg(test)]
    pub(super) fn from_test_parts(
        snapshot_id: CatalogSnapshotId,
        publication_digest: [u8; DIGEST_BYTES],
        content_digest: [u8; DIGEST_BYTES],
    ) -> Self {
        Self {
            snapshot_id,
            publication_digest,
            content_digest,
        }
    }

    pub(super) fn snapshot_id(self) -> CatalogSnapshotId {
        self.snapshot_id
    }

    pub(super) fn publication_digest(self) -> [u8; DIGEST_BYTES] {
        self.publication_digest
    }

    pub(super) fn content_digest(self) -> [u8; DIGEST_BYTES] {
        self.content_digest
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct CatalogReadyPublicationIdentity {
    header: CatalogReadyPublicationHeaderIdentity,
    project_rows: Vec<CatalogReadyRowCommitment>,
    session_rows: Vec<CatalogReadyRowCommitment>,
    resolution_index: CatalogResolutionIndex,
    member_history: CatalogPublicationMemberHistory,
    reducer: CatalogReducerPublication,
    refresh_depth: usize,
    retained_chain: Vec<CatalogRetainedSnapshotCommitment>,
}

impl std::fmt::Debug for CatalogReadyPublicationIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogReadyPublicationIdentity")
            .field("header", &self.header)
            .field("project_row_count", &self.project_rows.len())
            .field("session_row_count", &self.session_rows.len())
            .field("resolution_index", &self.resolution_index)
            .field("member_history", &self.member_history)
            .field("reducer", &self.reducer)
            .field("refresh_depth", &self.refresh_depth)
            .field("retained_snapshot_count", &self.retained_chain.len())
            .finish_non_exhaustive()
    }
}

impl CatalogReadyPublicationIdentity {
    pub(super) fn build_commit_seq(&self) -> u64 {
        self.header.build_commit_seq
    }

    pub(super) fn is_refresh(&self) -> bool {
        matches!(self.header.lineage, CatalogSnapshotLineage::Refresh { .. })
    }

    pub(super) fn contract_selection(&self) -> &ContractVersionSelection {
        &self.header.contract_selection
    }

    pub(super) fn resolution_index(&self) -> &CatalogResolutionIndex {
        &self.resolution_index
    }

    pub(super) fn refresh_predecessor(
        &self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<CatalogRefreshPredecessor, EngineError> {
        CatalogRefreshPredecessor::new(
            snapshot_id,
            self.header.publication_digest,
            self.header.content_digest,
            self.header.contract_selection.clone(),
            self.header.member_identity_contract_id.clone(),
            self.header.reducer_revision,
            self.member_history.revision(),
        )
        .map_err(catalog_state::catalog_contract_error)
    }

    pub(super) fn resume_reducer(&self) -> CatalogReducer {
        self.reducer.resume_for_refresh()
    }

    pub(super) fn reducer(&self) -> &CatalogReducerPublication {
        &self.reducer
    }

    pub(super) fn member_history(&self) -> &CatalogPublicationMemberHistory {
        &self.member_history
    }

    pub(super) fn refresh_depth(&self) -> usize {
        self.refresh_depth
    }

    pub(super) fn permits_refresh_successor(&self) -> bool {
        self.refresh_depth < MAX_RETAINED_REFRESH_LINEAGE_DEPTH
    }

    pub(super) fn retained_snapshot_count(&self) -> usize {
        self.retained_chain.len()
    }

    pub(super) fn retained_chain(&self) -> &[CatalogRetainedSnapshotCommitment] {
        &self.retained_chain
    }

    pub(super) fn retained_snapshot_commitment(
        &self,
        snapshot_id: CatalogSnapshotId,
    ) -> Option<CatalogRetainedSnapshotCommitment> {
        self.retained_chain
            .iter()
            .copied()
            .find(|commitment| commitment.snapshot_id == snapshot_id)
    }

    pub(super) fn matches_snapshot_commitment(
        &self,
        expected: CatalogRetainedSnapshotCommitment,
    ) -> bool {
        self.header.publication_digest == expected.publication_digest
            && self.header.content_digest == expected.content_digest
    }

    pub(super) fn matches_row(
        &self,
        kind: CatalogDurablePublicationEntryKind,
        key: &[u8; DIGEST_BYTES],
        payload_len: usize,
        payload_digest: &[u8; DIGEST_BYTES],
    ) -> bool {
        let commitments = match kind {
            CatalogDurablePublicationEntryKind::ProjectRow => &self.project_rows,
            CatalogDurablePublicationEntryKind::SessionRow => &self.session_rows,
            _ => return false,
        };
        let Ok(index) = commitments.binary_search_by_key(key, |commitment| commitment.key) else {
            return false;
        };
        let commitment = &commitments[index];
        usize::try_from(commitment.payload_len) == Ok(payload_len)
            && &commitment.payload_digest == payload_digest
    }

    pub(super) fn expected_row_keys(
        &self,
        kind: CatalogDurablePublicationEntryKind,
        after_key: Option<&[u8; DIGEST_BYTES]>,
        limit: usize,
    ) -> Option<Vec<[u8; DIGEST_BYTES]>> {
        let commitments = match kind {
            CatalogDurablePublicationEntryKind::ProjectRow => &self.project_rows,
            CatalogDurablePublicationEntryKind::SessionRow => &self.session_rows,
            _ => return None,
        };
        let start = match after_key {
            Some(key) => commitments
                .binary_search_by_key(key, |commitment| commitment.key)
                .ok()?
                .checked_add(1)?,
            None => 0,
        };
        Some(
            commitments[start..]
                .iter()
                .take(limit)
                .map(|commitment| commitment.key)
                .collect(),
        )
    }
}

pub(super) struct LoadedReadyPublication {
    pub(super) source_coverage: Vec<SourceCoverageSet>,
    pub(super) identity: CatalogReadyPublicationIdentity,
}

pub(super) fn apply_initial_catalog_publication(
    connection: &mut Connection,
    command: &CatalogInitialPublicationCommand,
) -> Result<Option<CatalogInitialPublicationReceipt>, EngineError> {
    apply_initial_catalog_publication_with_hook(
        connection,
        command,
        &NoopCatalogPublicationCommitHook,
    )
}

pub(super) fn apply_initial_catalog_publication_with_hook(
    connection: &mut Connection,
    command: &CatalogInitialPublicationCommand,
    hook: &dyn CatalogPublicationCommitHook,
) -> Result<Option<CatalogInitialPublicationReceipt>, EngineError> {
    if command.expected_build_commit_seq == 0 {
        return Err(EngineError::InvalidCommit(
            "catalog initial publication requires a positive Building commit expectation"
                .to_string(),
        ));
    }
    if command.committed_at < command.started_at {
        return Err(EngineError::InvalidCommit(
            "catalog initial publication commit time must not precede its start".to_string(),
        ));
    }
    // The complete representation and aggregate byte ceiling are checked
    // before SQLite begins, so oversized input cannot create a partial row.
    let durable = command
        .assembly
        .prepare_durable()
        .map_err(catalog_state::catalog_contract_error)?;

    hook.reach(CatalogPublicationCommitStage::BeforeTransaction)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| catalog_state::sqlite_error("begin catalog publication", error))?;
    let current = catalog_state::load_catalog_build_state(&transaction)?.ok_or_else(|| {
        EngineError::InvalidCommit(
            "catalog initial publication requires a registered Building lineage".to_string(),
        )
    })?;

    if current.readiness.state == CatalogReadinessPhase::Ready {
        validate_semantic_build(&current, &durable)?;
        let retained = load_ready_publication(
            &transaction,
            &current.plan,
            current
                .readiness
                .last_complete_snapshot
                .expect("validated Ready state has a snapshot"),
            current.readiness.attempt,
        )?;
        if retained.identity.header.build_commit_seq == command.expected_build_commit_seq
            && retained.identity.header.publication_digest
                == *durable.publication_digest().storage_bytes()
            && retained.identity.header.content_digest == *durable.content_digest()
        {
            transaction.commit().map_err(|error| {
                catalog_state::sqlite_error("finish unchanged catalog publication", error)
            })?;
            return Ok(None);
        }
        return Err(EngineError::InvalidCommit(
            "catalog initial publication conflicts with the retained Ready snapshot".to_string(),
        ));
    }
    validate_building_cas(&current, &durable, command.expected_build_commit_seq)?;

    let commit_seq = catalog_state::insert_administrative_commit(
        &transaction,
        catalog_state::INITIAL_PUBLICATION_REASON,
        command.started_at,
        command.committed_at,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterCommitInsert)?;
    let snapshot_id = CatalogSnapshotId::new(
        durable.build().desired_contract_version,
        durable.build().coverage_plan_id,
        durable.build().epoch,
        commit_seq,
    )
    .map_err(catalog_state::catalog_contract_error)?;
    insert_snapshot(
        &transaction,
        &durable,
        command.expected_build_commit_seq,
        snapshot_id,
        command.committed_at,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterSnapshotWrite)?;

    let mut ordinal = 0_u64;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[
            CatalogDurablePublicationEntryKind::Source,
            CatalogDurablePublicationEntryKind::MemberBinding,
        ],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterSourceEntries)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::ReducerState],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterEvidenceEntries)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::ProjectRow],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterProjectRows)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::SessionRow],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterSessionRows)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::Tombstone],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterTombstones)?;
    if ordinal != durable.entries().len() as u64 {
        return Err(EngineError::InvalidCommit(
            "catalog durable entry groups did not consume the complete publication".to_string(),
        ));
    }

    let mut machine =
        CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
            .map_err(catalog_state::catalog_contract_error)?;
    machine
        .publish_ready(snapshot_id, durable.source_coverage().to_vec())
        .map_err(catalog_state::catalog_contract_error)?;
    write_ready_state(
        &transaction,
        &current,
        command.expected_build_commit_seq,
        commit_seq,
        command.committed_at,
        machine.snapshot(),
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterReadinessWrite)?;
    write_ready_change(&transaction, commit_seq, snapshot_id, &durable)?;
    hook.reach(CatalogPublicationCommitStage::AfterOutboxInsert)?;
    hook.reach(CatalogPublicationCommitStage::BeforeCommit)?;
    transaction
        .commit()
        .map_err(|error| catalog_state::sqlite_error("commit catalog publication", error))?;
    hook.reach(CatalogPublicationCommitStage::AfterCommit)?;
    Ok(Some(CatalogInitialPublicationReceipt {
        commit_seq,
        snapshot_id,
        readiness: machine.snapshot().clone(),
    }))
}

pub(super) fn apply_refresh_catalog_publication(
    connection: &mut Connection,
    command: &CatalogRefreshPublicationCommand,
) -> Result<Option<CatalogRefreshPublicationReceipt>, EngineError> {
    apply_refresh_catalog_publication_with_hook(
        connection,
        command,
        &NoopCatalogPublicationCommitHook,
    )
}

pub(super) fn apply_refresh_catalog_publication_with_hook(
    connection: &mut Connection,
    command: &CatalogRefreshPublicationCommand,
    hook: &dyn CatalogPublicationCommitHook,
) -> Result<Option<CatalogRefreshPublicationReceipt>, EngineError> {
    if command.committed_at < command.started_at {
        return Err(EngineError::InvalidCommit(
            "catalog refresh publication commit time must not precede its start".to_string(),
        ));
    }
    let durable = command
        .assembly
        .prepare_durable()
        .map_err(catalog_state::catalog_contract_error)?;
    if durable.build().refresh_started_commit_seq != command.expected.refresh_started_commit_seq()
        || durable.build().predecessor_snapshot != command.expected.predecessor_snapshot()
        || durable.predecessor() != &command.expected.predecessor()?
    {
        return Err(EngineError::InvalidCommit(
            "catalog refresh publication assembly differs from its non-transferable expectation"
                .to_string(),
        ));
    }

    hook.reach(CatalogPublicationCommitStage::BeforeTransaction)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| catalog_state::sqlite_error("begin catalog refresh publication", error))?;
    let current = catalog_state::load_catalog_build_state(&transaction)?.ok_or_else(|| {
        EngineError::InvalidCommit(
            "catalog refresh publication requires an active Ready lineage".to_string(),
        )
    })?;
    if current.readiness.state == CatalogReadinessPhase::Ready
        && current.readiness.refreshing_from_snapshot.is_none()
    {
        let snapshot_id = current
            .readiness
            .last_complete_snapshot
            .ok_or_else(|| EngineError::InvalidCommit("Ready snapshot is missing".to_string()))?;
        let retained = load_ready_publication(
            &transaction,
            &current.plan,
            snapshot_id,
            current.readiness.attempt,
        )?;
        let exact_replay = matches!(
            &retained.identity.header.lineage,
            CatalogSnapshotLineage::Refresh {
                predecessor_snapshot,
                predecessor_publication_digest,
                predecessor_content_digest,
            } if *predecessor_snapshot == durable.build().predecessor_snapshot
                && predecessor_publication_digest == durable.predecessor().publication_digest()
                && predecessor_content_digest == durable.predecessor().content_digest()
        ) && retained.identity.header.build_commit_seq
            == durable.build().refresh_started_commit_seq
            && retained.identity.header.publication_digest
                == *durable.publication_digest().storage_bytes()
            && retained.identity.header.content_digest == *durable.content_digest();
        if exact_replay {
            transaction.commit().map_err(|error| {
                catalog_state::sqlite_error("finish unchanged catalog refresh publication", error)
            })?;
            return Ok(None);
        }
        return Err(EngineError::InvalidCommit(
            "catalog refresh publication conflicts with the retained Ready successor".to_string(),
        ));
    }
    let actual_expectation = current.refresh_publication_expectation()?;
    if actual_expectation != command.expected
        || actual_expectation.publication_identity() != command.expected.publication_identity()
    {
        return Err(EngineError::InvalidCommit(
            "catalog refresh publication compare-and-swap is stale or foreign".to_string(),
        ));
    }
    if !actual_expectation
        .publication_identity()
        .permits_refresh_successor()
    {
        return Err(EngineError::InvalidCommit(
            "catalog refresh publication would exceed the bounded retained lineage depth"
                .to_string(),
        ));
    }
    validate_refresh_semantics(&current, &durable)?;

    let commit_seq = catalog_state::insert_administrative_commit(
        &transaction,
        catalog_state::REFRESH_PUBLICATION_REASON,
        command.started_at,
        command.committed_at,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterCommitInsert)?;
    let snapshot_id = CatalogSnapshotId::new(
        durable.build().desired_contract_version,
        durable.build().coverage_plan_id,
        durable.build().epoch,
        commit_seq,
    )
    .map_err(catalog_state::catalog_contract_error)?;
    insert_refresh_snapshot(&transaction, &durable, snapshot_id, command.committed_at)?;
    hook.reach(CatalogPublicationCommitStage::AfterSnapshotWrite)?;

    let mut ordinal = 0_u64;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[
            CatalogDurablePublicationEntryKind::Source,
            CatalogDurablePublicationEntryKind::MemberBinding,
            CatalogDurablePublicationEntryKind::MemberHistory,
        ],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterSourceEntries)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::ReducerState],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterEvidenceEntries)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::ProjectRow],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterProjectRows)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::SessionRow],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterSessionRows)?;
    insert_entry_kinds(
        &transaction,
        commit_seq,
        durable.entries(),
        &[CatalogDurablePublicationEntryKind::Tombstone],
        &mut ordinal,
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterTombstones)?;
    if ordinal != durable.entries().len() as u64 {
        return Err(EngineError::InvalidCommit(
            "catalog refresh entry groups did not consume the complete publication".to_string(),
        ));
    }

    let mut machine =
        CatalogReadinessMachine::resume(current.plan.clone(), current.readiness.clone())
            .map_err(catalog_state::catalog_contract_error)?;
    machine
        .publish_ready(snapshot_id, durable.source_coverage().to_vec())
        .map_err(catalog_state::catalog_contract_error)?;
    write_refresh_ready_state(
        &transaction,
        &current,
        &command.expected,
        commit_seq,
        command.committed_at,
        machine.snapshot(),
    )?;
    hook.reach(CatalogPublicationCommitStage::AfterReadinessWrite)?;
    write_refresh_ready_change(&transaction, commit_seq, snapshot_id, &durable)?;
    hook.reach(CatalogPublicationCommitStage::AfterOutboxInsert)?;
    hook.reach(CatalogPublicationCommitStage::BeforeCommit)?;
    transaction.commit().map_err(|error| {
        catalog_state::sqlite_error("commit catalog refresh publication", error)
    })?;
    hook.reach(CatalogPublicationCommitStage::AfterCommit)?;
    Ok(Some(CatalogRefreshPublicationReceipt {
        commit_seq,
        predecessor_snapshot: durable.build().predecessor_snapshot,
        snapshot_id,
        readiness: machine.snapshot().clone(),
    }))
}

fn validate_refresh_semantics(
    current: &DurableCatalogBuildState,
    durable: &CatalogDurableRefreshPublication,
) -> Result<(), EngineError> {
    let expected = durable.build();
    let lineage_matches = if current.refresh_publication_expectation()?.is_recovery() {
        matches!(
            current.readiness.state,
            CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
        ) && current.readiness.refreshing_from_snapshot.is_none()
            && current.readiness.complete_through_commit.is_none()
            && current
                .readiness
                .reason
                .as_ref()
                .and_then(|reason| match reason {
                    crate::catalog_contract::CatalogReadinessReason::SourceRetrying { code } => {
                        Some(code.as_str())
                    }
                    _ => None,
                })
                == current
                    .refresh_publication_expectation()?
                    .retry_reason_code()
    } else {
        current.readiness.state == CatalogReadinessPhase::Ready
            && current.readiness.refreshing_from_snapshot == Some(expected.predecessor_snapshot)
            && current.readiness.complete_through_commit
                == Some(expected.predecessor_snapshot.complete_commit)
            && current
                .readiness
                .reason
                .as_ref()
                .and_then(|reason| match reason {
                    crate::catalog_contract::CatalogReadinessReason::SourceRetrying { code } => {
                        Some(code.as_str())
                    }
                    _ => None,
                })
                == current
                    .refresh_publication_expectation()?
                    .retry_reason_code()
    };
    if current.plan.scope != CatalogCoverageScope::Library
        || current.plan.coverage_plan_id != expected.coverage_plan_id
        || current.readiness.scope != CatalogCoverageScope::Library
        || current.readiness.coverage_plan_id != expected.coverage_plan_id
        || current.readiness.desired_contract_version != expected.desired_contract_version
        || current.readiness.completed_contract_version != Some(expected.desired_contract_version)
        || current.readiness.epoch != expected.epoch
        || current.readiness.attempt != expected.attempt
        || current.readiness.last_complete_snapshot != Some(expected.predecessor_snapshot)
        || !lineage_matches
        || current.last_commit_seq != expected.refresh_started_commit_seq
    {
        return Err(EngineError::InvalidCommit(
            "catalog refresh publication semantic expectation is stale or foreign".to_string(),
        ));
    }
    Ok(())
}

fn validate_semantic_build(
    current: &DurableCatalogBuildState,
    durable: &CatalogDurableInitialPublication,
) -> Result<(), EngineError> {
    let expected = durable.build();
    if current.plan.scope != CatalogCoverageScope::Library
        || current.plan.coverage_plan_id != expected.coverage_plan_id
        || current.readiness.scope != CatalogCoverageScope::Library
        || current.readiness.coverage_plan_id != expected.coverage_plan_id
        || current.readiness.desired_contract_version != expected.desired_contract_version
        || current.readiness.epoch != expected.epoch
        || current.readiness.attempt != expected.attempt
    {
        return Err(EngineError::InvalidCommit(
            "catalog publication semantic build expectation is stale or foreign".to_string(),
        ));
    }
    Ok(())
}

fn validate_building_cas(
    current: &DurableCatalogBuildState,
    durable: &CatalogDurableInitialPublication,
    expected_build_commit_seq: u64,
) -> Result<(), EngineError> {
    validate_semantic_build(current, durable)?;
    if !matches!(
        current.readiness.state,
        CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
    ) || current.last_commit_seq != expected_build_commit_seq
        || expected_build_commit_seq == 0
    {
        return Err(EngineError::InvalidCommit(
            "catalog initial publication Building compare-and-swap is stale or foreign".to_string(),
        ));
    }
    Ok(())
}

fn insert_snapshot(
    transaction: &Transaction<'_>,
    durable: &CatalogDurableInitialPublication,
    build_commit_seq: u64,
    snapshot_id: CatalogSnapshotId,
    published_at: i64,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO catalog_snapshots (
                snapshot_commit_seq, build_commit_seq,
                durable_publication_contract_version, pack_contract_version,
                coverage_plan_id, readiness_epoch, attempt,
                contract_selection_json, member_identity_contract_id,
                publication_digest, reducer_revision, entries_digest,
                content_digest, entry_count, encoded_bytes, source_count,
                member_count, project_row_count, session_row_count,
                tombstone_count, published_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21
            )
            "#,
            params![
                catalog_state::to_i64(snapshot_id.complete_commit, "catalog snapshot commit")?,
                catalog_state::to_i64(build_commit_seq, "catalog Building commit")?,
                i64::from(durable.contract_version()),
                i64::from(snapshot_id.pack_contract_version),
                snapshot_id.coverage_plan_id.storage_bytes().as_slice(),
                catalog_state::to_i64(snapshot_id.readiness_epoch, "catalog snapshot epoch")?,
                catalog_state::to_i64(durable.build().attempt, "catalog snapshot attempt")?,
                durable.contract_selection_json(),
                durable.member_identity_contract_id(),
                durable.publication_digest().storage_bytes().as_slice(),
                durable.reducer_revision().storage_bytes().as_slice(),
                durable.entries_digest().as_slice(),
                durable.content_digest().as_slice(),
                catalog_state::to_i64(durable.entries().len() as u64, "catalog entry count")?,
                catalog_state::to_i64(durable.encoded_bytes() as u64, "catalog encoded bytes")?,
                catalog_state::to_i64(durable.source_count() as u64, "catalog source count")?,
                catalog_state::to_i64(durable.member_count() as u64, "catalog member count")?,
                catalog_state::to_i64(
                    durable.project_row_count() as u64,
                    "catalog project row count",
                )?,
                catalog_state::to_i64(
                    durable.session_row_count() as u64,
                    "catalog session row count",
                )?,
                catalog_state::to_i64(
                    durable.tombstone_count() as u64,
                    "catalog tombstone count",
                )?,
                published_at,
            ],
        )
        .map_err(|error| catalog_state::sqlite_error("insert catalog snapshot", error))?;
    Ok(())
}

fn insert_refresh_snapshot(
    transaction: &Transaction<'_>,
    durable: &CatalogDurableRefreshPublication,
    snapshot_id: CatalogSnapshotId,
    published_at: i64,
) -> Result<(), EngineError> {
    transaction
        .execute(
            r#"
            INSERT INTO catalog_snapshots (
                snapshot_commit_seq, build_commit_seq,
                durable_publication_contract_version, pack_contract_version,
                coverage_plan_id, readiness_epoch, attempt,
                contract_selection_json, member_identity_contract_id,
                publication_digest, reducer_revision, entries_digest,
                content_digest, entry_count, encoded_bytes, source_count,
                member_count, project_row_count, session_row_count,
                tombstone_count, replaces_snapshot_commit_seq,
                replaces_publication_digest, replaces_content_digest,
                published_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
            )
            "#,
            params![
                catalog_state::to_i64(snapshot_id.complete_commit, "catalog refresh snapshot")?,
                catalog_state::to_i64(
                    durable.build().refresh_started_commit_seq,
                    "catalog refresh-start commit"
                )?,
                i64::from(durable.contract_version()),
                i64::from(snapshot_id.pack_contract_version),
                snapshot_id.coverage_plan_id.storage_bytes().as_slice(),
                catalog_state::to_i64(snapshot_id.readiness_epoch, "catalog refresh epoch")?,
                catalog_state::to_i64(durable.build().attempt, "catalog refresh attempt")?,
                durable.contract_selection_json(),
                durable.member_identity_contract_id(),
                durable.publication_digest().storage_bytes().as_slice(),
                durable.reducer_revision().storage_bytes().as_slice(),
                durable.entries_digest().as_slice(),
                durable.content_digest().as_slice(),
                catalog_state::to_i64(durable.entries().len() as u64, "catalog refresh entries")?,
                catalog_state::to_i64(durable.encoded_bytes() as u64, "catalog refresh bytes")?,
                catalog_state::to_i64(durable.source_count() as u64, "catalog refresh sources")?,
                catalog_state::to_i64(durable.member_count() as u64, "catalog refresh members")?,
                catalog_state::to_i64(
                    durable.project_row_count() as u64,
                    "catalog refresh project rows"
                )?,
                catalog_state::to_i64(
                    durable.session_row_count() as u64,
                    "catalog refresh session rows"
                )?,
                catalog_state::to_i64(
                    durable.tombstone_count() as u64,
                    "catalog refresh tombstones"
                )?,
                catalog_state::to_i64(
                    durable.build().predecessor_snapshot.complete_commit,
                    "catalog refresh predecessor"
                )?,
                durable.predecessor().publication_digest().as_slice(),
                durable.predecessor().content_digest().as_slice(),
                published_at,
            ],
        )
        .map_err(|error| catalog_state::sqlite_error("insert catalog refresh snapshot", error))?;
    Ok(())
}

fn insert_entry_kinds(
    transaction: &Transaction<'_>,
    snapshot_commit_seq: u64,
    entries: &[crate::catalog_contract::publication::CatalogDurablePublicationEntry],
    kinds: &[CatalogDurablePublicationEntryKind],
    ordinal: &mut u64,
) -> Result<(), EngineError> {
    let mut statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO catalog_snapshot_entries (
                snapshot_commit_seq, ordinal, entry_kind, entry_key,
                payload, payload_digest
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .map_err(|error| catalog_state::sqlite_error("prepare catalog snapshot entry", error))?;
    for entry in entries.iter().filter(|entry| kinds.contains(&entry.kind())) {
        statement
            .execute(params![
                catalog_state::to_i64(snapshot_commit_seq, "catalog snapshot commit")?,
                catalog_state::to_i64(*ordinal, "catalog snapshot entry ordinal")?,
                entry.kind().as_str(),
                entry.key().as_slice(),
                entry.payload(),
                entry.payload_digest().as_slice(),
            ])
            .map_err(|error| catalog_state::sqlite_error("insert catalog snapshot entry", error))?;
        *ordinal = ordinal.checked_add(1).ok_or_else(|| {
            EngineError::InvalidCommit("catalog snapshot entry ordinal overflow".to_string())
        })?;
    }
    Ok(())
}

fn write_ready_state(
    transaction: &Transaction<'_>,
    current: &DurableCatalogBuildState,
    expected_build_commit_seq: u64,
    commit_seq: u64,
    updated_at: i64,
    readiness: &CatalogReadinessSnapshot,
) -> Result<(), EngineError> {
    if readiness.state != CatalogReadinessPhase::Ready
        || readiness.complete_through_commit != Some(commit_seq)
        || readiness
            .last_complete_snapshot
            .map(|snapshot| snapshot.complete_commit)
            != Some(commit_seq)
        || readiness.completed_contract_version != Some(readiness.desired_contract_version)
    {
        return Err(EngineError::InvalidCommit(
            "catalog Ready transition does not identify the publication commit".to_string(),
        ));
    }
    let changed = transaction
        .execute(
            r#"
            UPDATE catalog_build_state
            SET state = 'ready', completed_contract_version = ?1,
                complete_through_commit = ?2,
                last_complete_snapshot_commit = ?2,
                reason_code = NULL,
                last_commit_seq = ?2, updated_at = ?3
            WHERE scope_kind = ?4
              AND coverage_plan_id = ?5
              AND desired_contract_version = ?6
              AND epoch = ?7
              AND attempt = ?8
              AND state IN ('building', 'partial')
              AND last_commit_seq = ?9
            "#,
            params![
                i64::from(readiness.desired_contract_version),
                catalog_state::to_i64(commit_seq, "catalog Ready commit")?,
                updated_at,
                catalog_state::LIBRARY_SCOPE,
                current.plan.coverage_plan_id.storage_bytes().as_slice(),
                i64::from(readiness.desired_contract_version),
                catalog_state::to_i64(readiness.epoch, "catalog readiness epoch")?,
                catalog_state::to_i64(readiness.attempt, "catalog readiness attempt")?,
                catalog_state::to_i64(expected_build_commit_seq, "catalog Building commit")?,
            ],
        )
        .map_err(|error| catalog_state::sqlite_error("publish catalog Ready state", error))?;
    if changed != 1 {
        return Err(EngineError::InvalidCommit(
            "catalog Ready compare-and-swap changed no row".to_string(),
        ));
    }
    Ok(())
}

fn write_refresh_ready_state(
    transaction: &Transaction<'_>,
    current: &DurableCatalogBuildState,
    expected: &CatalogActiveRefreshPublicationExpectation,
    commit_seq: u64,
    updated_at: i64,
    readiness: &CatalogReadinessSnapshot,
) -> Result<(), EngineError> {
    let predecessor = expected.predecessor_snapshot();
    if readiness.state != CatalogReadinessPhase::Ready
        || readiness.refreshing_from_snapshot.is_some()
        || readiness.complete_through_commit != Some(commit_seq)
        || readiness
            .last_complete_snapshot
            .map(|snapshot| snapshot.complete_commit)
            != Some(commit_seq)
        || commit_seq <= predecessor.complete_commit
    {
        return Err(EngineError::InvalidCommit(
            "catalog refresh Ready transition does not identify one newer publication".to_string(),
        ));
    }
    let changed = if expected.is_recovery() {
        transaction
            .execute(
                r#"
                UPDATE catalog_build_state
                SET state = 'ready',
                    complete_through_commit = ?1,
                    last_complete_snapshot_commit = ?1,
                    refreshing_from_snapshot_commit = NULL,
                    reason_code = NULL,
                    last_commit_seq = ?1, updated_at = ?2
                WHERE scope_kind = ?3
                  AND coverage_plan_id = ?4
                  AND desired_contract_version = ?5
                  AND epoch = ?6
                  AND attempt = ?7
                  AND state = ?11
                  AND completed_contract_version = ?5
                  AND complete_through_commit IS NULL
                  AND last_complete_snapshot_commit = ?8
                  AND refreshing_from_snapshot_commit IS NULL
                  AND reason_code IS ?9
                  AND last_commit_seq = ?10
                "#,
                params![
                    catalog_state::to_i64(commit_seq, "catalog recovery Ready commit")?,
                    updated_at,
                    catalog_state::LIBRARY_SCOPE,
                    current.plan.coverage_plan_id.storage_bytes().as_slice(),
                    i64::from(readiness.desired_contract_version),
                    catalog_state::to_i64(readiness.epoch, "catalog recovery epoch")?,
                    catalog_state::to_i64(readiness.attempt, "catalog recovery attempt")?,
                    catalog_state::to_i64(
                        predecessor.complete_commit,
                        "catalog recovery predecessor commit"
                    )?,
                    expected.retry_reason_code(),
                    catalog_state::to_i64(
                        expected.refresh_started_commit_seq(),
                        "catalog recovery-start commit"
                    )?,
                    expected.durable_state().as_str(),
                ],
            )
            .map_err(|error| catalog_state::sqlite_error("publish catalog recovery Ready", error))?
    } else {
        transaction
            .execute(
                r#"
            UPDATE catalog_build_state
            SET complete_through_commit = ?1,
                last_complete_snapshot_commit = ?1,
                refreshing_from_snapshot_commit = NULL,
                reason_code = NULL,
                last_commit_seq = ?1, updated_at = ?2
            WHERE scope_kind = ?3
              AND coverage_plan_id = ?4
              AND desired_contract_version = ?5
              AND epoch = ?6
              AND attempt = ?7
              AND state = 'ready'
              AND completed_contract_version = ?5
              AND complete_through_commit = ?8
              AND last_complete_snapshot_commit = ?8
              AND refreshing_from_snapshot_commit = ?8
              AND reason_code IS ?9
              AND last_commit_seq = ?10
            "#,
                params![
                    catalog_state::to_i64(commit_seq, "catalog refresh Ready commit")?,
                    updated_at,
                    catalog_state::LIBRARY_SCOPE,
                    current.plan.coverage_plan_id.storage_bytes().as_slice(),
                    i64::from(readiness.desired_contract_version),
                    catalog_state::to_i64(readiness.epoch, "catalog refresh epoch")?,
                    catalog_state::to_i64(readiness.attempt, "catalog refresh attempt")?,
                    catalog_state::to_i64(
                        predecessor.complete_commit,
                        "catalog predecessor commit"
                    )?,
                    expected.retry_reason_code(),
                    catalog_state::to_i64(
                        expected.refresh_started_commit_seq(),
                        "catalog refresh-start commit"
                    )?,
                ],
            )
            .map_err(|error| catalog_state::sqlite_error("publish catalog refresh Ready", error))?
    };
    if changed != 1 {
        return Err(EngineError::InvalidCommit(
            "catalog refresh Ready compare-and-swap changed no row".to_string(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogReadyChangedPayload {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: String,
    desired_contract_version: u32,
    completed_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: &'static str,
    snapshot_id: CatalogSnapshotId,
    publication_digest: String,
    snapshot_content_digest: String,
    project_row_count: usize,
    session_row_count: usize,
    tombstone_count: usize,
    commit_seq: u64,
}

fn write_ready_change(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    snapshot_id: CatalogSnapshotId,
    durable: &CatalogDurableInitialPublication,
) -> Result<(), EngineError> {
    let payload = serde_json::to_vec(&CatalogReadyChangedPayload {
        readiness_contract_version: crate::catalog_contract::CATALOG_READINESS_CONTRACT_VERSION,
        scope: catalog_state::LIBRARY_SCOPE,
        coverage_plan_id: encoded_digest(snapshot_id.coverage_plan_id.storage_bytes()),
        desired_contract_version: snapshot_id.pack_contract_version,
        completed_contract_version: snapshot_id.pack_contract_version,
        epoch: snapshot_id.readiness_epoch,
        attempt: durable.build().attempt,
        state: "ready",
        snapshot_id,
        publication_digest: encoded_digest(durable.publication_digest().storage_bytes()),
        snapshot_content_digest: encoded_digest(durable.content_digest()),
        project_row_count: durable.project_row_count(),
        session_row_count: durable.session_row_count(),
        tombstone_count: durable.tombstone_count(),
        commit_seq,
    })
    .map_err(|error| {
        EngineError::InvalidCommit(format!(
            "could not encode catalog Ready invalidation: {error}"
        ))
    })?;
    commit::write_internal_changes(
        transaction,
        commit_seq,
        &[ChangeEntry {
            topic: READINESS_CHANGE_TOPIC.to_string(),
            schema_version: READY_CHANGE_SCHEMA_VERSION,
            entity_key: snapshot_id.coverage_plan_id.storage_bytes().to_vec(),
            operation: "upsert".to_string(),
            payload,
        }],
    )
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogRefreshReadyChangedPayload {
    readiness_contract_version: u32,
    scope: &'static str,
    coverage_plan_id: String,
    desired_contract_version: u32,
    completed_contract_version: u32,
    epoch: u64,
    attempt: u64,
    state: &'static str,
    predecessor_snapshot: CatalogSnapshotId,
    predecessor_publication_digest: String,
    predecessor_content_digest: String,
    snapshot_id: CatalogSnapshotId,
    publication_digest: String,
    snapshot_content_digest: String,
    project_row_count: usize,
    session_row_count: usize,
    tombstone_count: usize,
    commit_seq: u64,
}

fn write_refresh_ready_change(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    snapshot_id: CatalogSnapshotId,
    durable: &CatalogDurableRefreshPublication,
) -> Result<(), EngineError> {
    let predecessor = durable.predecessor();
    let payload = serde_json::to_vec(&CatalogRefreshReadyChangedPayload {
        readiness_contract_version: crate::catalog_contract::CATALOG_READINESS_CONTRACT_VERSION,
        scope: catalog_state::LIBRARY_SCOPE,
        coverage_plan_id: encoded_digest(snapshot_id.coverage_plan_id.storage_bytes()),
        desired_contract_version: snapshot_id.pack_contract_version,
        completed_contract_version: snapshot_id.pack_contract_version,
        epoch: snapshot_id.readiness_epoch,
        attempt: durable.build().attempt,
        state: "ready",
        predecessor_snapshot: predecessor.snapshot_id(),
        predecessor_publication_digest: encoded_digest(predecessor.publication_digest()),
        predecessor_content_digest: encoded_digest(predecessor.content_digest()),
        snapshot_id,
        publication_digest: encoded_digest(durable.publication_digest().storage_bytes()),
        snapshot_content_digest: encoded_digest(durable.content_digest()),
        project_row_count: durable.project_row_count(),
        session_row_count: durable.session_row_count(),
        tombstone_count: durable.tombstone_count(),
        commit_seq,
    })
    .map_err(|error| {
        EngineError::InvalidCommit(format!(
            "could not encode catalog refresh Ready invalidation: {error}"
        ))
    })?;
    commit::write_internal_changes(
        transaction,
        commit_seq,
        &[ChangeEntry {
            topic: READINESS_CHANGE_TOPIC.to_string(),
            schema_version: REFRESH_READY_CHANGE_SCHEMA_VERSION,
            entity_key: snapshot_id.coverage_plan_id.storage_bytes().to_vec(),
            operation: "upsert".to_string(),
            payload,
        }],
    )
}

fn encoded_digest(bytes: &[u8; DIGEST_BYTES]) -> String {
    format!("v1:{}", URL_SAFE_NO_PAD.encode(bytes))
}

struct StoredSnapshot {
    build_commit_seq: i64,
    durable_contract_version: i64,
    pack_contract_version: i64,
    coverage_plan_id: Option<Vec<u8>>,
    readiness_epoch: i64,
    attempt: i64,
    contract_selection_json: Option<Vec<u8>>,
    member_identity_contract_is_null: i64,
    member_identity_contract_id: Option<String>,
    publication_digest: Option<Vec<u8>>,
    reducer_revision: Option<Vec<u8>>,
    entries_digest: Option<Vec<u8>>,
    content_digest: Option<Vec<u8>>,
    entry_count: i64,
    encoded_bytes: i64,
    source_count: i64,
    member_count: i64,
    project_row_count: i64,
    session_row_count: i64,
    tombstone_count: i64,
    replaces_snapshot_commit_seq: Option<i64>,
    replaces_publication_digest: Option<Vec<u8>>,
    replaces_content_digest: Option<Vec<u8>>,
    published_at: i64,
}

struct StoredQueryHeader {
    build_commit_seq: i64,
    durable_contract_version: i64,
    pack_contract_version: i64,
    coverage_plan_id: Option<Vec<u8>>,
    readiness_epoch: i64,
    attempt: i64,
    contract_selection_json: Option<Vec<u8>>,
    member_identity_contract_is_null: i64,
    member_identity_contract_id: Option<String>,
    publication_digest: Option<Vec<u8>>,
    reducer_revision: Option<Vec<u8>>,
    entries_digest: Option<Vec<u8>>,
    content_digest: Option<Vec<u8>>,
    entry_count: i64,
    encoded_bytes: i64,
    source_count: i64,
    member_count: i64,
    project_row_count: i64,
    session_row_count: i64,
    tombstone_count: i64,
    replaces_snapshot_commit_seq: Option<i64>,
    replaces_publication_digest: Option<Vec<u8>>,
    replaces_content_digest: Option<Vec<u8>>,
    published_at: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct CatalogRetainedQueryHeader {
    pub(super) contract_selection: ContractVersionSelection,
    pub(super) project_row_count: usize,
    pub(super) session_row_count: usize,
    pub(super) encoded_bytes: usize,
}

/// Load only the bounded immutable header needed by a retained-page range
/// read. Absence is an internal not-retained result; it is deliberately not a
/// caller-visible SnapshotExpired claim because this slice has no retention
/// authority.
pub(super) fn load_retained_query_header(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    snapshot_id: CatalogSnapshotId,
    expected_attempt: u64,
    expected_identity: &CatalogReadyPublicationIdentity,
) -> Result<CatalogRetainedQueryHeader, EngineError> {
    plan.validate()
        .map_err(catalog_state::catalog_contract_error)?;
    let stored = connection
        .query_row(
            r#"
            SELECT build_commit_seq, durable_publication_contract_version,
                   pack_contract_version,
                   CASE WHEN typeof(coverage_plan_id) = 'blob'
                                  AND length(coverage_plan_id) = 32
                        THEN coverage_plan_id END,
                   readiness_epoch, attempt,
                   CASE WHEN typeof(contract_selection_json) = 'blob'
                                  AND length(contract_selection_json) BETWEEN 1 AND 4194304
                        THEN contract_selection_json END,
                   CASE WHEN member_identity_contract_id IS NULL THEN 1 ELSE 0 END,
                   CASE WHEN typeof(member_identity_contract_id) = 'text'
                                  AND length(CAST(member_identity_contract_id AS BLOB))
                                      BETWEEN 1 AND 256
                        THEN member_identity_contract_id END,
                   CASE WHEN typeof(publication_digest) = 'blob'
                                  AND length(publication_digest) = 32
                        THEN publication_digest END,
                   CASE WHEN typeof(reducer_revision) = 'blob'
                                  AND length(reducer_revision) = 32
                        THEN reducer_revision END,
                   CASE WHEN typeof(entries_digest) = 'blob'
                                  AND length(entries_digest) = 32
                        THEN entries_digest END,
                   CASE WHEN typeof(content_digest) = 'blob'
                                  AND length(content_digest) = 32
                        THEN content_digest END,
                   entry_count, encoded_bytes, source_count, member_count,
                   project_row_count, session_row_count, tombstone_count,
                   replaces_snapshot_commit_seq,
                   CASE WHEN typeof(replaces_publication_digest) = 'blob'
                                  AND length(replaces_publication_digest) = 32
                        THEN replaces_publication_digest END,
                   CASE WHEN typeof(replaces_content_digest) = 'blob'
                                  AND length(replaces_content_digest) = 32
                        THEN replaces_content_digest END,
                   published_at
            FROM catalog_snapshots WHERE snapshot_commit_seq = ?1
            "#,
            [catalog_state::to_i64(
                snapshot_id.complete_commit,
                "catalog retained snapshot commit",
            )?],
            |row| {
                Ok(StoredQueryHeader {
                    build_commit_seq: row.get(0)?,
                    durable_contract_version: row.get(1)?,
                    pack_contract_version: row.get(2)?,
                    coverage_plan_id: row.get(3)?,
                    readiness_epoch: row.get(4)?,
                    attempt: row.get(5)?,
                    contract_selection_json: row.get(6)?,
                    member_identity_contract_is_null: row.get(7)?,
                    member_identity_contract_id: row.get(8)?,
                    publication_digest: row.get(9)?,
                    reducer_revision: row.get(10)?,
                    entries_digest: row.get(11)?,
                    content_digest: row.get(12)?,
                    entry_count: row.get(13)?,
                    encoded_bytes: row.get(14)?,
                    source_count: row.get(15)?,
                    member_count: row.get(16)?,
                    project_row_count: row.get(17)?,
                    session_row_count: row.get(18)?,
                    tombstone_count: row.get(19)?,
                    replaces_snapshot_commit_seq: row.get(20)?,
                    replaces_publication_digest: row.get(21)?,
                    replaces_content_digest: row.get(22)?,
                    published_at: row.get(23)?,
                })
            },
        )
        .optional()
        .map_err(|error| catalog_state::sqlite_error("load retained catalog header", error))?
        .ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog snapshot is not retained by this database".to_string(),
            )
        })?;

    let coverage_plan_id = stored.coverage_plan_id.as_deref().ok_or_else(|| {
        catalog_state::corrupt_catalog_state(
            "retained catalog coverage-plan ID exceeds its fixed bound",
        )
    })?;
    let build_commit_seq =
        catalog_state::positive_u64(stored.build_commit_seq, "catalog Building commit")?;
    let durable_contract_version = catalog_state::positive_u32(
        stored.durable_contract_version,
        "catalog durable publication contract version",
    )?;
    let lineage = validate_snapshot_lineage(
        connection,
        snapshot_id,
        build_commit_seq,
        durable_contract_version,
        stored.replaces_snapshot_commit_seq,
        stored.replaces_publication_digest.as_deref(),
        stored.replaces_content_digest.as_deref(),
        stored.published_at,
    )?;
    if build_commit_seq >= snapshot_id.complete_commit
        || stored.pack_contract_version != i64::from(snapshot_id.pack_contract_version)
        || coverage_plan_id != plan.coverage_plan_id.storage_bytes()
        || snapshot_id.coverage_plan_id != plan.coverage_plan_id
        || stored.readiness_epoch
            != catalog_state::to_i64(snapshot_id.readiness_epoch, "catalog retained epoch")?
        || stored.attempt != catalog_state::to_i64(expected_attempt, "catalog retained attempt")?
        || plan.scope != CatalogCoverageScope::Library
    {
        return Err(catalog_state::corrupt_catalog_state(
            "retained catalog header is outside its exact plan/build lineage",
        ));
    }
    let publication_digest = decode_digest_column(
        stored.publication_digest.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "retained catalog publication digest exceeds its fixed bound",
            )
        })?,
        "catalog publication digest",
    )?;
    let reducer_revision = decode_digest_column(
        stored.reducer_revision.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "retained catalog reducer revision exceeds its fixed bound",
            )
        })?,
        "catalog reducer revision",
    )?;
    let entries_digest = decode_digest_column(
        stored.entries_digest.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "retained catalog entries digest exceeds its fixed bound",
            )
        })?,
        "catalog entries digest",
    )?;
    let content_digest = decode_digest_column(
        stored.content_digest.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "retained catalog content digest exceeds its fixed bound",
            )
        })?,
        "catalog content digest",
    )?;
    let contract_selection_json = stored.contract_selection_json.ok_or_else(|| {
        catalog_state::corrupt_catalog_state(
            "retained catalog selection exceeds its durable byte bound",
        )
    })?;
    let contract_selection: ContractVersionSelection =
        serde_json::from_slice(&contract_selection_json).map_err(|error| {
            catalog_state::corrupt_catalog_state(format!(
                "retained catalog selection is invalid: {error}"
            ))
        })?;
    validate_durable_contract_selection(&contract_selection)
        .map_err(catalog_state::catalog_contract_error)?;
    if contract_selection.query_pack_version != Some(snapshot_id.pack_contract_version) {
        return Err(catalog_state::corrupt_catalog_state(
            "retained catalog selection does not match its snapshot pack",
        ));
    }
    let entry_count = bounded_usize(
        stored.entry_count,
        1,
        MAX_DURABLE_PUBLICATION_ENTRIES,
        "retained catalog entry count",
    )?;
    let encoded_bytes = bounded_usize(
        stored.encoded_bytes,
        1,
        MAX_DURABLE_PUBLICATION_BYTES,
        "retained catalog encoded bytes",
    )?;
    let project_row_count = bounded_usize(
        stored.project_row_count,
        0,
        1_000_000,
        "retained catalog project row count",
    )?;
    let session_row_count = bounded_usize(
        stored.session_row_count,
        0,
        1_000_000,
        "retained catalog session row count",
    )?;
    let source_count = bounded_usize(
        stored.source_count,
        0,
        4_096,
        "retained catalog source count",
    )?;
    let member_count = bounded_usize(
        stored.member_count,
        0,
        1_000_000,
        "retained catalog member count",
    )?;
    let tombstone_count = bounded_usize(
        stored.tombstone_count,
        0,
        1_000_000,
        "retained catalog tombstone count",
    )?;
    let member_identity_contract_id = match (
        stored.member_identity_contract_is_null,
        stored.member_identity_contract_id,
    ) {
        (1, None) => None,
        (0, Some(value)) => {
            validate_member_identity_contract(&value)?;
            Some(value)
        }
        _ => {
            return Err(catalog_state::corrupt_catalog_state(
                "retained member identity contract has an invalid bounded projection",
            ));
        }
    };
    if member_identity_contract_id.is_some() != (source_count > 0) {
        return Err(catalog_state::corrupt_catalog_state(
            "retained member identity contract presence differs from its sources",
        ));
    }
    if project_row_count
        .checked_add(session_row_count)
        .is_none_or(|count| count > entry_count)
    {
        return Err(catalog_state::corrupt_catalog_state(
            "retained catalog row counts exceed the bounded entry count",
        ));
    }
    let header_identity = CatalogReadyPublicationHeaderIdentity {
        build_commit_seq,
        durable_contract_version,
        lineage,
        contract_selection: contract_selection.clone(),
        member_identity_contract_id,
        publication_digest,
        reducer_revision: CatalogReducerPublicationRevision::from_storage_bytes(reducer_revision),
        entries_digest,
        content_digest,
        entry_count,
        encoded_bytes,
        source_count,
        member_count,
        project_row_count,
        session_row_count,
        tombstone_count,
    };
    if header_identity != expected_identity.header {
        return Err(catalog_state::corrupt_catalog_state(
            "retained catalog header differs from the restart-validated publication identity",
        ));
    }
    Ok(CatalogRetainedQueryHeader {
        contract_selection,
        project_row_count,
        session_row_count,
        encoded_bytes,
    })
}

pub(super) fn load_ready_publication(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    snapshot_id: CatalogSnapshotId,
    expected_attempt: u64,
) -> Result<LoadedReadyPublication, EngineError> {
    load_ready_publication_at_depth(connection, plan, snapshot_id, expected_attempt, 0)
}

fn load_ready_publication_at_depth(
    connection: &Connection,
    plan: &CatalogCoveragePlan,
    snapshot_id: CatalogSnapshotId,
    expected_attempt: u64,
    refresh_depth: usize,
) -> Result<LoadedReadyPublication, EngineError> {
    let stored = connection
        .query_row(
            r#"
            SELECT build_commit_seq, durable_publication_contract_version,
                   pack_contract_version,
                   CASE WHEN typeof(coverage_plan_id) = 'blob'
                                  AND length(coverage_plan_id) = 32
                        THEN coverage_plan_id END,
                   readiness_epoch,
                   attempt,
                   CASE WHEN typeof(contract_selection_json) = 'blob'
                                  AND length(contract_selection_json) BETWEEN 1 AND 4194304
                        THEN contract_selection_json END,
                   CASE WHEN member_identity_contract_id IS NULL THEN 1 ELSE 0 END,
                   CASE WHEN typeof(member_identity_contract_id) = 'text'
                                  AND length(CAST(member_identity_contract_id AS BLOB))
                                      BETWEEN 1 AND 256
                        THEN member_identity_contract_id END,
                   CASE WHEN typeof(publication_digest) = 'blob'
                                  AND length(publication_digest) = 32
                        THEN publication_digest END,
                   CASE WHEN typeof(reducer_revision) = 'blob'
                                  AND length(reducer_revision) = 32
                        THEN reducer_revision END,
                   CASE WHEN typeof(entries_digest) = 'blob'
                                  AND length(entries_digest) = 32
                        THEN entries_digest END,
                   CASE WHEN typeof(content_digest) = 'blob'
                                  AND length(content_digest) = 32
                        THEN content_digest END,
                   entry_count, encoded_bytes, source_count, member_count,
                   project_row_count, session_row_count, tombstone_count,
                   replaces_snapshot_commit_seq,
                   CASE WHEN typeof(replaces_publication_digest) = 'blob'
                                  AND length(replaces_publication_digest) = 32
                        THEN replaces_publication_digest END,
                   CASE WHEN typeof(replaces_content_digest) = 'blob'
                                  AND length(replaces_content_digest) = 32
                        THEN replaces_content_digest END,
                   published_at
            FROM catalog_snapshots WHERE snapshot_commit_seq = ?1
            "#,
            [catalog_state::to_i64(
                snapshot_id.complete_commit,
                "catalog snapshot commit",
            )?],
            |row| {
                Ok(StoredSnapshot {
                    build_commit_seq: row.get(0)?,
                    durable_contract_version: row.get(1)?,
                    pack_contract_version: row.get(2)?,
                    coverage_plan_id: row.get(3)?,
                    readiness_epoch: row.get(4)?,
                    attempt: row.get(5)?,
                    contract_selection_json: row.get(6)?,
                    member_identity_contract_is_null: row.get(7)?,
                    member_identity_contract_id: row.get(8)?,
                    publication_digest: row.get(9)?,
                    reducer_revision: row.get(10)?,
                    entries_digest: row.get(11)?,
                    content_digest: row.get(12)?,
                    entry_count: row.get(13)?,
                    encoded_bytes: row.get(14)?,
                    source_count: row.get(15)?,
                    member_count: row.get(16)?,
                    project_row_count: row.get(17)?,
                    session_row_count: row.get(18)?,
                    tombstone_count: row.get(19)?,
                    replaces_snapshot_commit_seq: row.get(20)?,
                    replaces_publication_digest: row.get(21)?,
                    replaces_content_digest: row.get(22)?,
                    published_at: row.get(23)?,
                })
            },
        )
        .optional()
        .map_err(|error| catalog_state::sqlite_error("load catalog snapshot", error))?
        .ok_or_else(|| catalog_state::corrupt_catalog_state("Ready state snapshot is missing"))?;
    let coverage_plan_id = stored.coverage_plan_id.as_deref().ok_or_else(|| {
        catalog_state::corrupt_catalog_state(
            "catalog snapshot coverage-plan ID exceeds its fixed durable bound",
        )
    })?;
    let member_identity_contract_id = match (
        stored.member_identity_contract_is_null,
        stored.member_identity_contract_id.clone(),
    ) {
        (1, None) => None,
        (0, Some(value)) => Some(value),
        (0, None) => {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog snapshot member identity contract exceeds its durable byte bound",
            ));
        }
        _ => {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog snapshot member identity contract has an invalid NULL projection",
            ));
        }
    };
    let build_commit_seq =
        catalog_state::positive_u64(stored.build_commit_seq, "catalog snapshot Building commit")?;
    let durable_contract_version = catalog_state::positive_u32(
        stored.durable_contract_version,
        "catalog durable publication contract version",
    )?;
    let lineage = validate_snapshot_lineage(
        connection,
        snapshot_id,
        build_commit_seq,
        durable_contract_version,
        stored.replaces_snapshot_commit_seq,
        stored.replaces_publication_digest.as_deref(),
        stored.replaces_content_digest.as_deref(),
        stored.published_at,
    )?;
    if build_commit_seq >= snapshot_id.complete_commit
        || stored.pack_contract_version != i64::from(snapshot_id.pack_contract_version)
        || coverage_plan_id != snapshot_id.coverage_plan_id.storage_bytes()
        || stored.readiness_epoch
            != catalog_state::to_i64(snapshot_id.readiness_epoch, "catalog snapshot epoch")?
        || stored.attempt != catalog_state::to_i64(expected_attempt, "catalog snapshot attempt")?
        || snapshot_id.coverage_plan_id != plan.coverage_plan_id
        || plan.scope != CatalogCoverageScope::Library
    {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot header is outside its exact plan/build lineage",
        ));
    }
    let contract_selection_json = stored.contract_selection_json.ok_or_else(|| {
        catalog_state::corrupt_catalog_state(
            "catalog snapshot contract selection exceeds its durable byte bound",
        )
    })?;
    let contract_selection: ContractVersionSelection =
        serde_json::from_slice(&contract_selection_json).map_err(|error| {
            catalog_state::corrupt_catalog_state(format!(
                "catalog snapshot contract selection is invalid: {error}"
            ))
        })?;
    validate_durable_contract_selection(&contract_selection)
        .map_err(catalog_state::catalog_contract_error)?;
    if contract_selection.query_pack_version != Some(snapshot_id.pack_contract_version) {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot selection does not match its pack version",
        ));
    }

    let publication_digest = decode_digest_column(
        stored.publication_digest.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "catalog publication digest exceeds its fixed durable bound",
            )
        })?,
        "catalog publication digest",
    )?;
    let reducer_revision = decode_digest_column(
        stored.reducer_revision.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "catalog reducer revision exceeds its fixed durable bound",
            )
        })?,
        "catalog reducer revision",
    )?;
    let expected_entries_digest = decode_digest_column(
        stored.entries_digest.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "catalog entries digest exceeds its fixed durable bound",
            )
        })?,
        "catalog entries digest",
    )?;
    let expected_content_digest = decode_digest_column(
        stored.content_digest.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "catalog content digest exceeds its fixed durable bound",
            )
        })?,
        "catalog content digest",
    )?;
    let entry_count = bounded_usize(
        stored.entry_count,
        1,
        MAX_DURABLE_PUBLICATION_ENTRIES,
        "catalog snapshot entry count",
    )?;
    let encoded_bytes = bounded_usize(
        stored.encoded_bytes,
        1,
        MAX_DURABLE_PUBLICATION_BYTES,
        "catalog snapshot encoded bytes",
    )?;
    let source_count = bounded_usize(stored.source_count, 0, 4_096, "catalog source count")?;
    let member_count = bounded_usize(stored.member_count, 0, 1_000_000, "catalog member count")?;
    if member_identity_contract_id.is_some() != (source_count > 0) {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot member identity contract presence does not match its sources",
        ));
    }
    if let Some(identity_contract) = member_identity_contract_id.as_deref() {
        validate_member_identity_contract(identity_contract)?;
    }
    let project_row_count = bounded_usize(
        stored.project_row_count,
        0,
        1_000_000,
        "catalog project row count",
    )?;
    let session_row_count = bounded_usize(
        stored.session_row_count,
        0,
        1_000_000,
        "catalog session row count",
    )?;
    let tombstone_count = bounded_usize(
        stored.tombstone_count,
        0,
        1_000_000,
        "catalog tombstone count",
    )?;

    let predecessor_publication = match &lineage {
        CatalogSnapshotLineage::Initial => None,
        CatalogSnapshotLineage::Refresh {
            predecessor_snapshot,
            predecessor_publication_digest,
            predecessor_content_digest,
        } => {
            if predecessor_snapshot.coverage_plan_id != snapshot_id.coverage_plan_id
                || predecessor_snapshot.pack_contract_version != snapshot_id.pack_contract_version
                || predecessor_snapshot.readiness_epoch > snapshot_id.readiness_epoch
            {
                return Err(catalog_state::corrupt_catalog_state(
                    "catalog refresh predecessor is outside its exact plan/build lineage",
                ));
            }
            if refresh_depth >= MAX_RETAINED_REFRESH_LINEAGE_DEPTH {
                return Err(catalog_state::corrupt_catalog_state(
                    "catalog refresh lineage exceeds its bounded retained depth",
                ));
            }
            let predecessor_attempt: i64 = connection
                .query_row(
                    "SELECT attempt FROM catalog_snapshots WHERE snapshot_commit_seq = ?1",
                    [catalog_state::to_i64(
                        predecessor_snapshot.complete_commit,
                        "catalog predecessor snapshot commit",
                    )?],
                    |row| row.get(0),
                )
                .map_err(|error| {
                    catalog_state::sqlite_error("load catalog predecessor attempt", error)
                })?;
            let predecessor_attempt =
                catalog_state::positive_u64(predecessor_attempt, "catalog predecessor attempt")?;
            let predecessor = load_ready_publication_at_depth(
                connection,
                plan,
                *predecessor_snapshot,
                predecessor_attempt,
                refresh_depth + 1,
            )?;
            if predecessor.identity.header.publication_digest != *predecessor_publication_digest
                || predecessor.identity.header.content_digest != *predecessor_content_digest
                || predecessor.identity.header.contract_selection != contract_selection
                || predecessor.identity.header.member_identity_contract_id
                    != member_identity_contract_id
            {
                return Err(catalog_state::corrupt_catalog_state(
                    "catalog refresh predecessor commitment differs from its retained publication",
                ));
            }
            Some(predecessor)
        }
    };

    let mut statement = connection
        .prepare(
            r#"
            SELECT ordinal,
                   CASE WHEN entry_kind IN (
                       'source', 'member_binding', 'member_history', 'reducer_state',
                       'project_row', 'session_row', 'tombstone'
                   ) THEN entry_kind END,
                   CASE WHEN typeof(entry_key) = 'blob'
                                  AND length(entry_key) = 32
                        THEN entry_key END,
                   CASE WHEN typeof(payload) = 'blob'
                                  AND length(payload) BETWEEN 1 AND
                                      CASE WHEN entry_kind IN ('project_row', 'session_row')
                                           THEN MIN(?3, ?4) ELSE ?3 END
                        THEN payload END,
                   CASE WHEN typeof(payload_digest) = 'blob'
                                  AND length(payload_digest) = 32
                        THEN payload_digest END
            FROM catalog_snapshot_entries
            WHERE snapshot_commit_seq = ?1
            ORDER BY ordinal
            LIMIT ?2
            "#,
        )
        .map_err(|error| catalog_state::sqlite_error("prepare catalog snapshot entries", error))?;
    let scan_limit = entry_count.checked_add(1).ok_or_else(|| {
        catalog_state::corrupt_catalog_state("catalog snapshot entry scan limit overflow")
    })?;
    let mut rows = statement
        .query(params![
            catalog_state::to_i64(snapshot_id.complete_commit, "catalog snapshot commit")?,
            catalog_state::to_i64(scan_limit as u64, "catalog snapshot entry scan limit")?,
            catalog_state::to_i64(encoded_bytes as u64, "catalog snapshot encoded bytes")?,
            catalog_state::to_i64(
                MAX_DURABLE_CATALOG_ROW_BYTES as u64,
                "catalog durable row byte ceiling",
            )?,
        ])
        .map_err(|error| catalog_state::sqlite_error("query catalog snapshot entries", error))?;
    let mut summaries = Vec::with_capacity(entry_count);
    let mut source_frames: Vec<CatalogDurableSourceFrame> = Vec::with_capacity(source_count);
    let mut member_bindings: Vec<CatalogPublicationMemberBinding> =
        Vec::with_capacity(member_count);
    let mut member_history = None;
    let mut reducer_restore = None;
    let mut tombstones = Vec::with_capacity(tombstone_count);
    let mut project_row_commitments = Vec::new();
    project_row_commitments
        .try_reserve_exact(project_row_count)
        .map_err(|_| {
            catalog_state::corrupt_catalog_state(
                "catalog project-row commitment bound cannot be reserved",
            )
        })?;
    let mut session_row_commitments = Vec::new();
    session_row_commitments
        .try_reserve_exact(session_row_count)
        .map_err(|_| {
            catalog_state::corrupt_catalog_state(
                "catalog session-row commitment bound cannot be reserved",
            )
        })?;
    let mut actual_encoded_bytes = contract_selection_json.len();
    if let Some(identity_contract) = member_identity_contract_id.as_deref() {
        actual_encoded_bytes = actual_encoded_bytes
            .checked_add(identity_contract.len())
            .ok_or_else(|| catalog_state::corrupt_catalog_state("catalog byte count overflow"))?;
    }
    let mut kind_counts = [0_usize; 7];
    let mut previous_coordinate = None;
    while let Some(row) = rows
        .next()
        .map_err(|error| catalog_state::sqlite_error("read catalog snapshot entry", error))?
    {
        if summaries.len() == entry_count {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog snapshot contains more entries than its bounded header",
            ));
        }
        let ordinal: i64 = row.get(0).map_err(|error| {
            catalog_state::sqlite_error("decode catalog snapshot entry ordinal", error)
        })?;
        if ordinal != summaries.len() as i64 {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog snapshot entry ordinals are not contiguous",
            ));
        }
        let kind_text: Option<String> = row.get(1).map_err(|error| {
            catalog_state::sqlite_error("decode catalog snapshot entry kind", error)
        })?;
        let kind_text = kind_text.ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "catalog snapshot entry kind is outside the closed durable vocabulary",
            )
        })?;
        let kind = CatalogDurablePublicationEntryKind::parse(&kind_text)
            .map_err(catalog_state::catalog_contract_error)?;
        let key = decode_digest_column(
            &row.get::<_, Option<Vec<u8>>>(2)
                .map_err(|error| {
                    catalog_state::sqlite_error("decode catalog snapshot entry key", error)
                })?
                .ok_or_else(|| {
                    catalog_state::corrupt_catalog_state(
                        "catalog snapshot entry key exceeds its fixed durable bound",
                    )
                })?,
            "catalog snapshot entry key",
        )?;
        let payload: Option<Vec<u8>> = row.get(3).map_err(|error| {
            catalog_state::sqlite_error("decode catalog snapshot entry payload", error)
        })?;
        let payload = payload.ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "catalog snapshot entry payload is outside its durable byte bound",
            )
        })?;
        let payload_digest = decode_digest_column(
            &row.get::<_, Option<Vec<u8>>>(4)
                .map_err(|error| {
                    catalog_state::sqlite_error("decode catalog snapshot payload digest", error)
                })?
                .ok_or_else(|| {
                    catalog_state::corrupt_catalog_state(
                        "catalog snapshot payload digest exceeds its fixed durable bound",
                    )
                })?,
            "catalog snapshot payload digest",
        )?;
        if blake3::hash(&payload).as_bytes() != &payload_digest {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog snapshot entry payload digest does not match its bytes",
            ));
        }
        let coordinate = (kind, key);
        if previous_coordinate.is_some_and(|previous| previous >= coordinate) {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog snapshot entries are not canonical and duplicate-free",
            ));
        }
        previous_coordinate = Some(coordinate);
        if kind == CatalogDurablePublicationEntryKind::ReducerState && key != reducer_revision {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog reducer-state frame key does not match the snapshot reducer revision",
            ));
        }
        match kind {
            CatalogDurablePublicationEntryKind::Source => {
                source_frames.push(
                    decode_durable_source_frame(&payload, &key, encoded_bytes)
                        .map_err(catalog_state::catalog_contract_error)?,
                );
            }
            CatalogDurablePublicationEntryKind::MemberBinding => {
                member_bindings.push(
                    decode_durable_member_binding_frame(&payload, &key, encoded_bytes)
                        .map_err(catalog_state::catalog_contract_error)?,
                );
            }
            CatalogDurablePublicationEntryKind::MemberHistory => {
                if durable_contract_version != CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION
                    || member_history.is_some()
                {
                    return Err(catalog_state::corrupt_catalog_state(
                        "catalog snapshot has an unexpected or duplicate member-history frame",
                    ));
                }
                member_history = Some(
                    decode_durable_member_history_frame(&payload, &key, encoded_bytes)
                        .map_err(catalog_state::catalog_contract_error)?,
                );
            }
            CatalogDurablePublicationEntryKind::ReducerState => {
                if reducer_restore.is_some() {
                    return Err(catalog_state::corrupt_catalog_state(
                        "catalog snapshot contains duplicate reducer state",
                    ));
                }
                reducer_restore = Some(
                    decode_durable_reducer_state(&payload, encoded_bytes)
                        .map_err(catalog_state::catalog_contract_error)?,
                );
            }
            CatalogDurablePublicationEntryKind::ProjectRow => {
                decode_durable_project_row(&payload, &key, MAX_DURABLE_CATALOG_ROW_BYTES)
                    .map_err(catalog_state::catalog_contract_error)?;
                project_row_commitments.push(CatalogReadyRowCommitment {
                    key,
                    payload_digest,
                    payload_len: u32::try_from(payload.len()).map_err(|_| {
                        catalog_state::corrupt_catalog_state(
                            "catalog project row exceeds its commitment length bound",
                        )
                    })?,
                });
            }
            CatalogDurablePublicationEntryKind::SessionRow => {
                decode_durable_session_row(&payload, &key, MAX_DURABLE_CATALOG_ROW_BYTES)
                    .map_err(catalog_state::catalog_contract_error)?;
                session_row_commitments.push(CatalogReadyRowCommitment {
                    key,
                    payload_digest,
                    payload_len: u32::try_from(payload.len()).map_err(|_| {
                        catalog_state::corrupt_catalog_state(
                            "catalog session row exceeds its commitment length bound",
                        )
                    })?,
                });
            }
            CatalogDurablePublicationEntryKind::Tombstone => {
                tombstones.push(
                    decode_durable_tombstone(&payload, &key, encoded_bytes)
                        .map_err(catalog_state::catalog_contract_error)?,
                );
            }
        }
        actual_encoded_bytes = actual_encoded_bytes
            .checked_add(payload.len())
            .and_then(|bytes| bytes.checked_add(DIGEST_BYTES * 2))
            .and_then(|bytes| bytes.checked_add(kind.as_str().len()))
            .ok_or_else(|| catalog_state::corrupt_catalog_state("catalog byte count overflow"))?;
        if actual_encoded_bytes > MAX_DURABLE_PUBLICATION_BYTES {
            return Err(catalog_state::corrupt_catalog_state(
                "catalog snapshot exceeds its aggregate durable byte bound",
            ));
        }
        kind_counts[kind_index(kind)] += 1;
        summaries.push((kind, key, payload.len(), payload_digest));
    }
    if summaries.len() != entry_count
        || actual_encoded_bytes != encoded_bytes
        || kind_counts[kind_index(CatalogDurablePublicationEntryKind::Source)] != source_count
        || kind_counts[kind_index(CatalogDurablePublicationEntryKind::MemberBinding)]
            != member_count
        || kind_counts[kind_index(CatalogDurablePublicationEntryKind::MemberHistory)]
            != usize::from(
                durable_contract_version == CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION,
            )
        || kind_counts[kind_index(CatalogDurablePublicationEntryKind::ReducerState)] != 1
        || kind_counts[kind_index(CatalogDurablePublicationEntryKind::ProjectRow)]
            != project_row_count
        || kind_counts[kind_index(CatalogDurablePublicationEntryKind::SessionRow)]
            != session_row_count
        || kind_counts[kind_index(CatalogDurablePublicationEntryKind::Tombstone)] != tombstone_count
        || project_row_commitments.len() != project_row_count
        || session_row_commitments.len() != session_row_count
    {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot entry counts or bytes do not match its header",
        ));
    }
    let actual_entries_digest = derive_durable_entries_digest(&summaries);
    if actual_entries_digest != expected_entries_digest {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot entries digest does not match its canonical frames",
        ));
    }
    let reducer_revision = CatalogReducerPublicationRevision::from_storage_bytes(reducer_revision);
    let reducer = reducer_restore
        .ok_or_else(|| catalog_state::corrupt_catalog_state("catalog reducer state is missing"))?
        .finish(
            tombstones,
            reducer_revision,
            CatalogReducerPublicationLimits::default(),
        )
        .map_err(catalog_state::catalog_contract_error)?;
    reducer
        .validate_durable_row_commitments(
            MAX_DURABLE_CATALOG_ROW_BYTES,
            project_row_commitments.len(),
            session_row_commitments.len(),
            |kind, key, payload_len, payload_digest| {
                let commitments = match kind {
                    CatalogEntityKind::Project => &project_row_commitments,
                    CatalogEntityKind::Session => &session_row_commitments,
                };
                let Ok(index) = commitments.binary_search_by_key(key, |entry| entry.key) else {
                    return false;
                };
                let commitment = &commitments[index];
                usize::try_from(commitment.payload_len) == Ok(payload_len)
                    && &commitment.payload_digest == payload_digest
            },
        )
        .map_err(catalog_state::catalog_contract_error)?;
    let resolution_index = reducer
        .resolution_index()
        .map_err(catalog_state::catalog_contract_error)?;
    let (mut source_coverage, member_history, content_digest, actual_refresh_depth) =
        match predecessor_publication.as_ref() {
            None => {
                let publication = CatalogInitialPublicationDigest::from_digest(publication_digest);
                let content_digest = derive_durable_content_digest(
                    CatalogInitialBuildExpectation {
                        coverage_plan_id: snapshot_id.coverage_plan_id,
                        desired_contract_version: snapshot_id.pack_contract_version,
                        epoch: snapshot_id.readiness_epoch,
                        attempt: expected_attempt,
                    },
                    &contract_selection_json,
                    member_identity_contract_id.as_deref(),
                    publication,
                    reducer_revision,
                    source_count,
                    member_count,
                    project_row_count,
                    session_row_count,
                    tombstone_count,
                    encoded_bytes,
                    actual_entries_digest,
                );
                let member_history =
                    CatalogPublicationMemberHistory::from_bindings(&member_bindings)
                        .map_err(catalog_state::catalog_contract_error)?;
                let source_coverage = validate_restarted_initial_publication(
                    plan,
                    &contract_selection,
                    member_identity_contract_id.as_deref(),
                    source_frames,
                    member_bindings,
                    &reducer,
                )
                .map_err(catalog_state::catalog_contract_error)?;
                (source_coverage, member_history, content_digest, 0)
            }
            Some(predecessor_publication) => {
                let predecessor_snapshot = match lineage {
                    CatalogSnapshotLineage::Refresh {
                        predecessor_snapshot,
                        ..
                    } => predecessor_snapshot,
                    CatalogSnapshotLineage::Initial => {
                        unreachable!("predecessor publication is present only for refresh lineage")
                    }
                };
                let predecessor = predecessor_publication
                    .identity
                    .refresh_predecessor(predecessor_snapshot)?;
                let member_history = member_history.ok_or_else(|| {
                    catalog_state::corrupt_catalog_state(
                        "catalog refresh member history is missing",
                    )
                })?;
                let publication = CatalogRefreshPublicationDigest::from_digest(publication_digest);
                let content_digest = derive_durable_refresh_content_digest(
                    CatalogRefreshBuildExpectation {
                        coverage_plan_id: snapshot_id.coverage_plan_id,
                        desired_contract_version: snapshot_id.pack_contract_version,
                        epoch: snapshot_id.readiness_epoch,
                        attempt: expected_attempt,
                        refresh_started_commit_seq: build_commit_seq,
                        predecessor_snapshot,
                        plan_replacement: false,
                    },
                    &predecessor,
                    &contract_selection_json,
                    member_identity_contract_id.as_deref(),
                    publication,
                    reducer_revision,
                    member_history.revision(),
                    source_count,
                    member_count,
                    project_row_count,
                    session_row_count,
                    tombstone_count,
                    encoded_bytes,
                    actual_entries_digest,
                );
                let source_coverage = validate_restarted_refresh_publication(
                    plan,
                    &contract_selection,
                    member_identity_contract_id.as_deref(),
                    source_frames,
                    member_bindings,
                    &member_history,
                    &reducer,
                    predecessor_publication.identity.member_history(),
                    predecessor_publication.identity.reducer(),
                )
                .map_err(catalog_state::catalog_contract_error)?;
                let actual_refresh_depth = predecessor_publication
                    .identity
                    .refresh_depth()
                    .checked_add(1)
                    .ok_or_else(|| {
                        catalog_state::corrupt_catalog_state(
                            "catalog refresh lineage depth overflow",
                        )
                    })?;
                (
                    source_coverage,
                    member_history,
                    content_digest,
                    actual_refresh_depth,
                )
            }
        };
    if content_digest != expected_content_digest {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot content digest does not match its header and entries",
        ));
    }
    let mut retained_chain = predecessor_publication
        .as_ref()
        .map(|predecessor| predecessor.identity.retained_chain.clone())
        .unwrap_or_default();
    retained_chain.push(CatalogRetainedSnapshotCommitment {
        snapshot_id,
        publication_digest,
        content_digest,
    });
    if retained_chain.len() != actual_refresh_depth + 1
        || retained_chain.len() > MAX_RETAINED_REFRESH_LINEAGE_DEPTH + 1
    {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog retained snapshot commitments exceed their bounded linear lineage",
        ));
    }
    source_coverage.sort_by(|left, right| {
        (&left.scope.adapter_id, left.scope.source_instance_key)
            .cmp(&(&right.scope.adapter_id, right.scope.source_instance_key))
    });
    Ok(LoadedReadyPublication {
        source_coverage,
        identity: CatalogReadyPublicationIdentity {
            header: CatalogReadyPublicationHeaderIdentity {
                build_commit_seq,
                durable_contract_version,
                lineage,
                contract_selection,
                member_identity_contract_id,
                publication_digest,
                reducer_revision,
                entries_digest: actual_entries_digest,
                content_digest,
                entry_count,
                encoded_bytes,
                source_count,
                member_count,
                project_row_count,
                session_row_count,
                tombstone_count,
            },
            project_rows: project_row_commitments,
            session_rows: session_row_commitments,
            resolution_index,
            member_history,
            reducer,
            refresh_depth: actual_refresh_depth,
            retained_chain,
        },
    })
}

fn validate_member_identity_contract(value: &str) -> Result<(), EngineError> {
    if value.is_empty() || value.trim() != value || value.len() > 256 {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog member identity contract is not a bounded canonical identifier",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_snapshot_lineage(
    connection: &Connection,
    snapshot_id: CatalogSnapshotId,
    build_commit_seq: u64,
    durable_contract_version: u32,
    replaces_snapshot_commit_seq: Option<i64>,
    replaces_publication_digest: Option<&[u8]>,
    replaces_content_digest: Option<&[u8]>,
    published_at: i64,
) -> Result<CatalogSnapshotLineage, EngineError> {
    match (
        durable_contract_version,
        replaces_snapshot_commit_seq,
        replaces_publication_digest,
        replaces_content_digest,
    ) {
        (CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION, None, None, None) => {
            validate_commit_owner_one_of(
                connection,
                build_commit_seq,
                &[
                    "catalog.library.build.scheduled",
                    catalog_state::PARTIAL_REASON,
                    catalog_state::INITIAL_SOURCE_RETRYING_REASON,
                    catalog_state::REFRESH_RECOVERY_STARTED_REASON,
                    catalog_state::SOURCE_GENERATION_INVALIDATED_REASON,
                ],
                None,
            )?;
            validate_commit_owner(
                connection,
                snapshot_id.complete_commit,
                catalog_state::INITIAL_PUBLICATION_REASON,
                Some(published_at),
            )?;
            Ok(CatalogSnapshotLineage::Initial)
        }
        (
            CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION,
            Some(predecessor_commit),
            Some(predecessor_publication_digest),
            Some(predecessor_content_digest),
        ) => {
            let predecessor_commit = catalog_state::positive_u64(
                predecessor_commit,
                "catalog refresh predecessor commit",
            )?;
            if predecessor_commit >= build_commit_seq
                || build_commit_seq >= snapshot_id.complete_commit
            {
                return Err(catalog_state::corrupt_catalog_state(
                    "catalog refresh snapshot commit order is not strictly increasing",
                ));
            }
            validate_commit_owner_one_of(
                connection,
                build_commit_seq,
                &[
                    "catalog.library.refresh.started",
                    catalog_state::REFRESH_SOURCE_RETRYING_REASON,
                    catalog_state::REFRESH_RECOVERY_STARTED_REASON,
                    catalog_state::PARTIAL_REASON,
                    catalog_state::SOURCE_GENERATION_INVALIDATED_REASON,
                ],
                None,
            )?;
            validate_commit_owner(
                connection,
                snapshot_id.complete_commit,
                catalog_state::REFRESH_PUBLICATION_REASON,
                Some(published_at),
            )?;
            let (predecessor_pack, predecessor_plan, predecessor_epoch): (
                i64,
                Option<Vec<u8>>,
                i64,
            ) = connection
                .query_row(
                    r#"
                    SELECT pack_contract_version,
                           CASE WHEN typeof(coverage_plan_id) = 'blob'
                                          AND length(coverage_plan_id) = 32
                                THEN coverage_plan_id END,
                           readiness_epoch
                    FROM catalog_snapshots WHERE snapshot_commit_seq = ?1
                    "#,
                    [catalog_state::to_i64(
                        predecessor_commit,
                        "catalog refresh predecessor commit",
                    )?],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|error| {
                    catalog_state::sqlite_error("load catalog predecessor identity", error)
                })?;
            let predecessor_pack =
                catalog_state::positive_u32(predecessor_pack, "catalog predecessor pack version")?;
            let predecessor_epoch = catalog_state::positive_u64(
                predecessor_epoch,
                "catalog predecessor readiness epoch",
            )?;
            if predecessor_pack != snapshot_id.pack_contract_version
                || predecessor_plan.as_deref()
                    != Some(snapshot_id.coverage_plan_id.storage_bytes().as_slice())
                || predecessor_epoch > snapshot_id.readiness_epoch
            {
                return Err(catalog_state::corrupt_catalog_state(
                    "catalog refresh predecessor is outside its exact plan/build lineage",
                ));
            }
            Ok(CatalogSnapshotLineage::Refresh {
                predecessor_snapshot: CatalogSnapshotId::new(
                    predecessor_pack,
                    snapshot_id.coverage_plan_id,
                    predecessor_epoch,
                    predecessor_commit,
                )
                .map_err(catalog_state::catalog_contract_error)?,
                predecessor_publication_digest: decode_digest_column(
                    predecessor_publication_digest,
                    "catalog predecessor publication digest",
                )?,
                predecessor_content_digest: decode_digest_column(
                    predecessor_content_digest,
                    "catalog predecessor content digest",
                )?,
            })
        }
        _ => Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot durable version and predecessor fields disagree",
        )),
    }
}

fn validate_commit_owner(
    connection: &Connection,
    commit_seq: u64,
    expected_reason: &str,
    expected_committed_at: Option<i64>,
) -> Result<(), EngineError> {
    validate_commit_owner_one_of(
        connection,
        commit_seq,
        &[expected_reason],
        expected_committed_at,
    )
}

fn validate_commit_owner_one_of(
    connection: &Connection,
    commit_seq: u64,
    expected_reasons: &[&str],
    expected_committed_at: Option<i64>,
) -> Result<(), EngineError> {
    let (source, reason, committed_at, fact_count): (Option<i64>, String, Option<i64>, i64) =
        connection
            .query_row(
                r#"
                SELECT source_instance_id, reason, committed_at, fact_count
                FROM ingest_commits WHERE commit_seq = ?1
                "#,
                [catalog_state::to_i64(commit_seq, "catalog lineage commit")?],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|error| catalog_state::sqlite_error("load catalog lineage commit", error))?;
    if source.is_some()
        || !expected_reasons.contains(&reason.as_str())
        || committed_at.is_none()
        || fact_count != 0
    {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog publication lineage is not owned by the expected source-neutral commit",
        ));
    }
    if expected_committed_at.is_some() && committed_at != expected_committed_at {
        return Err(catalog_state::corrupt_catalog_state(
            "catalog snapshot timestamp does not match its owning commit",
        ));
    }
    Ok(())
}

fn decode_digest_column(
    bytes: &[u8],
    field: &'static str,
) -> Result<[u8; DIGEST_BYTES], EngineError> {
    let digest: [u8; DIGEST_BYTES] = bytes.try_into().map_err(|_| {
        catalog_state::corrupt_catalog_state(format!("{field} is not {DIGEST_BYTES} bytes"))
    })?;
    if digest.iter().all(|byte| *byte == 0) {
        return Err(catalog_state::corrupt_catalog_state(format!(
            "{field} must be nonzero"
        )));
    }
    Ok(digest)
}

fn bounded_usize(
    value: i64,
    minimum: usize,
    maximum: usize,
    field: &'static str,
) -> Result<usize, EngineError> {
    let value = usize::try_from(value).map_err(|_| {
        catalog_state::corrupt_catalog_state(format!("{field} is negative or too large"))
    })?;
    if value < minimum || value > maximum {
        return Err(catalog_state::corrupt_catalog_state(format!(
            "{field} is outside {minimum}..={maximum}"
        )));
    }
    Ok(value)
}

const fn kind_index(kind: CatalogDurablePublicationEntryKind) -> usize {
    match kind {
        CatalogDurablePublicationEntryKind::Source => 0,
        CatalogDurablePublicationEntryKind::MemberBinding => 1,
        CatalogDurablePublicationEntryKind::MemberHistory => 2,
        CatalogDurablePublicationEntryKind::ReducerState => 3,
        CatalogDurablePublicationEntryKind::ProjectRow => 4,
        CatalogDurablePublicationEntryKind::SessionRow => 5,
        CatalogDurablePublicationEntryKind::Tombstone => 6,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::adapter::{
        CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey, ContractCompleteness,
        CoverageAbsence, CoverageAbsenceKind, CoverageDeclarationDigest, CoverageDomain,
        CoverageMembershipRevision, CoverageObjectKey, CoveragePosition, CoveragePositionKind,
        CoverageProvenance, CoverageSetCompleteness, CoverageStatus, CoverageStreamKey,
        FactRevisionId, QualifiedValue, QualifiedValueQuality, SemanticRevisionRef,
        SourceCoveragePoint, CONTRACT_VERSION_SELECTION_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
    };
    use crate::catalog_contract::evidence::{
        CatalogAvailability, CatalogDisclosureClass, CatalogEntityRef, CatalogEvidenceOwner,
        CatalogFieldAuthority, CatalogProjectAssertion, CatalogQualifiedField, CatalogReducer,
        CatalogRetractionCause, CatalogRetractionEvidence, CatalogSessionAssertion,
    };
    use crate::catalog_contract::publication::{
        CatalogCompleteSourceAssembly, CatalogPublicationLimits, CatalogPublicationMemberRef,
        CatalogSourceCompletionRevision, CatalogSourceMembershipRevision,
    };
    use crate::catalog_contract::{
        CatalogAccessPolicyDigest, CatalogCoveragePlanSource, CATALOG_PROJECTION_PACK_ID,
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
    };
    use crate::core::schema;
    use crate::engine::catalog_retention::CatalogSnapshotRetirementCommand;
    use crate::engine::catalog_state::CatalogBuildStateCommand;
    use crate::engine::writer::WriterRuntime;
    use crate::engine::{EngineOptions, SpaghettiEngineCore};
    use tempfile::tempdir;

    const PRECOMMIT_STAGES: [CatalogPublicationCommitStage; 11] = [
        CatalogPublicationCommitStage::BeforeTransaction,
        CatalogPublicationCommitStage::AfterCommitInsert,
        CatalogPublicationCommitStage::AfterSnapshotWrite,
        CatalogPublicationCommitStage::AfterSourceEntries,
        CatalogPublicationCommitStage::AfterEvidenceEntries,
        CatalogPublicationCommitStage::AfterProjectRows,
        CatalogPublicationCommitStage::AfterSessionRows,
        CatalogPublicationCommitStage::AfterTombstones,
        CatalogPublicationCommitStage::AfterReadinessWrite,
        CatalogPublicationCommitStage::AfterOutboxInsert,
        CatalogPublicationCommitStage::BeforeCommit,
    ];

    struct FailAt(CatalogPublicationCommitStage);

    impl CatalogPublicationCommitHook for FailAt {
        fn reach(&self, stage: CatalogPublicationCommitStage) -> Result<(), EngineError> {
            if stage == self.0 {
                Err(EngineError::InjectedFailure {
                    stage: stage_name(stage),
                })
            } else {
                Ok(())
            }
        }
    }

    struct AssertExternalBuilding {
        database_path: PathBuf,
        target: CatalogPublicationCommitStage,
        observed: Cell<bool>,
    }

    struct AssertExternalRefreshing {
        database_path: PathBuf,
        predecessor_snapshot: CatalogSnapshotId,
        predecessor_entries: i64,
        observed: Cell<bool>,
    }

    impl CatalogPublicationCommitHook for AssertExternalRefreshing {
        fn reach(&self, stage: CatalogPublicationCommitStage) -> Result<(), EngineError> {
            if stage != CatalogPublicationCommitStage::AfterReadinessWrite {
                return Ok(());
            }
            let connection =
                Connection::open(&self.database_path).map_err(|error| EngineError::Sqlite {
                    operation: "open external catalog refresh visibility check",
                    detail: error.to_string(),
                })?;
            let (state, refreshing, snapshots, entries): (String, Option<i64>, i64, i64) =
                connection
                    .query_row(
                        r#"
                        SELECT state, refreshing_from_snapshot_commit,
                               (SELECT COUNT(*) FROM catalog_snapshots),
                               (SELECT COUNT(*) FROM catalog_snapshot_entries)
                        FROM catalog_build_state WHERE scope_kind = 'library'
                        "#,
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .map_err(|error| {
                        catalog_state::sqlite_error("read external catalog refresh state", error)
                    })?;
            if state != "ready"
                || refreshing
                    != Some(catalog_state::to_i64(
                        self.predecessor_snapshot.complete_commit,
                        "catalog predecessor snapshot",
                    )?)
                || snapshots != 1
                || entries != self.predecessor_entries
            {
                return Err(EngineError::InvalidCommit(
                    "uncommitted catalog refresh publication became externally visible".to_owned(),
                ));
            }
            self.observed.set(true);
            Ok(())
        }
    }

    impl CatalogPublicationCommitHook for AssertExternalBuilding {
        fn reach(&self, stage: CatalogPublicationCommitStage) -> Result<(), EngineError> {
            if stage != self.target {
                return Ok(());
            }
            let connection =
                Connection::open(&self.database_path).map_err(|error| EngineError::Sqlite {
                    operation: "open external catalog visibility check",
                    detail: error.to_string(),
                })?;
            let state: String = connection
                .query_row(
                    "SELECT state FROM catalog_build_state WHERE scope_kind = 'library'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| {
                    catalog_state::sqlite_error("read external catalog state", error)
                })?;
            let snapshots: i64 = connection
                .query_row("SELECT COUNT(*) FROM catalog_snapshots", [], |row| {
                    row.get(0)
                })
                .map_err(|error| {
                    catalog_state::sqlite_error("count external catalog snapshots", error)
                })?;
            let entries: i64 = connection
                .query_row("SELECT COUNT(*) FROM catalog_snapshot_entries", [], |row| {
                    row.get(0)
                })
                .map_err(|error| {
                    catalog_state::sqlite_error("count external catalog entries", error)
                })?;
            if state != "building" || snapshots != 0 || entries != 0 {
                return Err(EngineError::InvalidCommit(
                    "uncommitted catalog publication became externally visible".to_owned(),
                ));
            }
            self.observed.set(true);
            Ok(())
        }
    }

    fn stage_name(stage: CatalogPublicationCommitStage) -> &'static str {
        match stage {
            CatalogPublicationCommitStage::BeforeTransaction => {
                "before catalog publication transaction"
            }
            CatalogPublicationCommitStage::AfterCommitInsert => {
                "after catalog publication commit insert"
            }
            CatalogPublicationCommitStage::AfterSnapshotWrite => "after catalog snapshot header",
            CatalogPublicationCommitStage::AfterSourceEntries => "after catalog source entries",
            CatalogPublicationCommitStage::AfterEvidenceEntries => "after catalog evidence entries",
            CatalogPublicationCommitStage::AfterProjectRows => "after catalog project rows",
            CatalogPublicationCommitStage::AfterSessionRows => "after catalog session rows",
            CatalogPublicationCommitStage::AfterTombstones => "after catalog tombstones",
            CatalogPublicationCommitStage::AfterReadinessWrite => "after catalog readiness write",
            CatalogPublicationCommitStage::AfterOutboxInsert => "after catalog outbox insert",
            CatalogPublicationCommitStage::BeforeCommit => "before catalog publication commit",
            CatalogPublicationCommitStage::AfterCommit => "after catalog publication commit",
        }
    }

    fn selection() -> ContractVersionSelection {
        ContractVersionSelection {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_version: SOURCE_COVERAGE_CONTRACT_VERSION,
            fact_family_versions: BTreeMap::from([("catalog.session".to_owned(), 1)]),
            query_pack_version: Some(CATALOG_QUERY_PACK_CONTRACT_VERSION),
            observation_contract_version: None,
        }
    }

    fn plan_source() -> CatalogCoveragePlanSource {
        CatalogCoveragePlanSource::new(
            "fixture-agent",
            CanonicalSourceInstanceKey::derive(1, b"private-source-instance").unwrap(),
            "fixture-support@candidate-v1",
            CoverageDeclarationDigest::derive(b"fixture-catalog-declaration-v1").unwrap(),
            CatalogAccessPolicyDigest::derive(1, b"private-library-policy").unwrap(),
        )
        .unwrap()
    }

    fn second_plan_source() -> CatalogCoveragePlanSource {
        CatalogCoveragePlanSource::new(
            "fixture-agent-two",
            CanonicalSourceInstanceKey::derive(1, b"private-source-instance-two").unwrap(),
            "fixture-support@candidate-v1",
            CoverageDeclarationDigest::derive(b"fixture-catalog-declaration-two-v1").unwrap(),
            CatalogAccessPolicyDigest::derive(1, b"private-library-policy-two").unwrap(),
        )
        .unwrap()
    }

    fn plan() -> CatalogCoveragePlan {
        CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![plan_source()],
            Vec::new(),
        )
        .unwrap()
    }

    fn complete_source(
        completion_label: &[u8],
    ) -> Result<CatalogCompleteSourceAssembly, crate::catalog_contract::CatalogContractError> {
        complete_plan_source(plan_source(), completion_label)
    }

    fn complete_plan_source(
        source: CatalogCoveragePlanSource,
        completion_label: &[u8],
    ) -> Result<CatalogCompleteSourceAssembly, crate::catalog_contract::CatalogContractError> {
        let domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
            version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
        };
        let coverage = SourceCoverageSet::new(
            domain,
            source.coverage_scope(CatalogCoverageScope::Library),
            CoverageMembershipRevision::derive(b"private-complete-membership").unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .unwrap();
        CatalogCompleteSourceAssembly::from_complete_library_coverage(
            source,
            selection(),
            "catalog-session-identity-v1",
            CatalogSourceMembershipRevision::from_digest(
                *blake3::hash(b"private-membership-revision").as_bytes(),
            ),
            CatalogSourceCompletionRevision::from_digest(
                *blake3::hash(completion_label).as_bytes(),
            ),
            Vec::new(),
            coverage,
        )
    }

    fn semantic_revision(owner: &CatalogEvidenceOwner, label: &str) -> SemanticRevisionRef {
        let fact_id = CanonicalFactId::native(
            &owner.adapter_id,
            &owner.source_instance_key,
            "catalog.publication.persistence-fixture",
            label.as_bytes(),
        )
        .unwrap();
        SemanticRevisionRef::new(FactRevisionId::derive(&fact_id, 1, label.as_bytes()).unwrap())
    }

    fn availability(
        owner: &CatalogEvidenceOwner,
        label: &str,
    ) -> CatalogQualifiedField<CatalogAvailability> {
        CatalogQualifiedField::new(
            QualifiedValue::from_parts(
                Some(CatalogAvailability::HistoryReady),
                QualifiedValueQuality::Exact,
                CatalogFieldAuthority::new("catalog-membership", 100, true).unwrap(),
                ContractCompleteness::Complete,
                None,
                None,
                vec![semantic_revision(owner, label)],
            )
            .unwrap(),
            CatalogDisclosureClass::Public,
        )
        .unwrap()
    }

    fn rich_assembly(
        coverage_plan: &CatalogCoveragePlan,
        readiness: &CatalogReadinessSnapshot,
    ) -> CatalogInitialPublicationAssembly {
        let source = plan_source();
        let stream_key = CoverageStreamKey::derive("fixture-agent", b"catalog-rich").unwrap();
        let live_object = CoverageObjectKey::derive("catalog-rich", b"live-object").unwrap();
        let deleted_object = CoverageObjectKey::derive("catalog-rich", b"deleted-object").unwrap();
        let live_owner = CatalogEvidenceOwner::new(
            "fixture-agent",
            source.source_instance_key,
            stream_key,
            live_object,
            1,
        )
        .unwrap();
        let deleted_owner = CatalogEvidenceOwner::new(
            "fixture-agent",
            source.source_instance_key,
            stream_key,
            deleted_object,
            1,
        )
        .unwrap();
        let domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
            version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
        };
        let point = SourceCoveragePoint::new(
            domain.clone(),
            "fixture-agent",
            source.source_instance_key,
            stream_key,
            live_object,
            1,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::SnapshotRevision,
                    b"rich-live-position",
                    None,
                )
                .unwrap(),
            ),
            CoverageStatus::ExactSnapshot,
            CoverageProvenance::default(),
        )
        .unwrap();
        let coverage = SourceCoverageSet::new(
            domain,
            source.coverage_scope(CatalogCoverageScope::Library),
            CoverageMembershipRevision::derive(b"rich-complete-membership").unwrap(),
            vec![point],
            vec![CoverageAbsence {
                stream_key,
                object_key: deleted_object,
                generation: 1,
                kind: CoverageAbsenceKind::Deleted,
            }],
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .unwrap();
        let member_ref =
            CatalogPublicationMemberRef::from_digest(*blake3::hash(b"rich-live-member").as_bytes());
        let source_assembly = CatalogCompleteSourceAssembly::from_complete_library_coverage(
            source,
            selection(),
            "catalog-session-identity-v1",
            CatalogSourceMembershipRevision::from_digest(
                *blake3::hash(b"rich-membership-revision").as_bytes(),
            ),
            CatalogSourceCompletionRevision::from_digest(
                *blake3::hash(b"rich-completion-revision").as_bytes(),
            ),
            vec![member_ref],
            coverage,
        )
        .unwrap();

        let project_ref = CatalogEntityRef::project(
            CanonicalEntityKey::derive(
                "fixture-agent",
                &live_owner.source_instance_key,
                "project",
                b"rich-project",
            )
            .unwrap(),
        );
        let session_ref = CatalogEntityRef::session(
            CanonicalEntityKey::derive(
                "fixture-agent",
                &live_owner.source_instance_key,
                "session",
                b"rich-session",
            )
            .unwrap(),
        );
        let deleted_ref = CatalogEntityRef::session(
            CanonicalEntityKey::derive(
                "fixture-agent",
                &deleted_owner.source_instance_key,
                "session",
                b"rich-deleted-session",
            )
            .unwrap(),
        );
        let project_assertion = CatalogProjectAssertion::new(
            live_owner.clone(),
            b"rich-project-assertion",
            project_ref,
            None,
            None,
            None,
            None,
            None,
            availability(&live_owner, "rich-project-availability"),
            vec![semantic_revision(&live_owner, "rich-project")],
        )
        .unwrap();
        let session_assertion = CatalogSessionAssertion::new(
            live_owner.clone(),
            b"rich-session-assertion",
            session_ref,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            availability(&live_owner, "rich-session-availability"),
            vec![semantic_revision(&live_owner, "rich-session")],
        )
        .unwrap();
        let deleted_assertion = CatalogSessionAssertion::new(
            deleted_owner.clone(),
            b"rich-deleted-assertion",
            deleted_ref,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            availability(&deleted_owner, "rich-deleted-availability"),
            vec![semantic_revision(&deleted_owner, "rich-deleted")],
        )
        .unwrap();
        let session_assertion_key = session_assertion.assertion_key;
        let deletion = CatalogRetractionEvidence::new(
            deleted_owner.clone(),
            CatalogRetractionCause::ConfirmedDeletion,
            ContractCompleteness::Complete,
            vec![semantic_revision(&deleted_owner, "rich-deletion")],
        )
        .unwrap();
        let mut reducer = CatalogReducer::default();
        reducer
            .upsert_project_assertion(project_assertion, 10)
            .unwrap();
        reducer
            .upsert_session_assertion(session_assertion, 10)
            .unwrap();
        reducer
            .upsert_session_assertion(deleted_assertion, 10)
            .unwrap();
        reducer.retract_owner(&deletion, 20).unwrap();
        reducer.confirm_absent(deleted_ref, &deletion, 21).unwrap();
        let binding = source_assembly
            .member_binding(member_ref, session_assertion_key, session_ref)
            .unwrap();
        CatalogInitialPublicationAssembly::assemble(
            coverage_plan,
            readiness,
            selection(),
            vec![source_assembly],
            &reducer,
            vec![binding],
            CatalogPublicationLimits::default(),
        )
        .unwrap()
    }

    fn assemble(
        plan: &CatalogCoveragePlan,
        readiness: &CatalogReadinessSnapshot,
        completion_label: &[u8],
    ) -> CatalogInitialPublicationAssembly {
        CatalogInitialPublicationAssembly::assemble(
            plan,
            readiness,
            selection(),
            vec![complete_source(completion_label).unwrap()],
            &CatalogReducer::default(),
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap()
    }

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
    }

    fn prepare_building(
        connection: &mut Connection,
        completion_label: &[u8],
    ) -> (CatalogCoveragePlan, CatalogInitialPublicationCommand) {
        let coverage_plan = plan();
        let registered = catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        assert_eq!(registered.commit_seq, 1);
        let pending = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        let scheduled = catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        assert_eq!(scheduled.commit_seq, 2);
        let assembly = assemble(&coverage_plan, &scheduled.readiness, completion_label);
        (
            coverage_plan,
            CatalogInitialPublicationCommand::new(assembly, scheduled.commit_seq, 30, 31),
        )
    }

    fn prepare_cold_replacement(
        connection: &mut Connection,
        completion_label: &[u8],
    ) -> (CatalogCoveragePlan, CatalogInitialPublicationCommand) {
        let coverage_plan = plan();
        catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        let pending = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let building = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        let replacement = catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::invalidate_source_generation(
                building
                    .source_generation_invalidation_expectation()
                    .unwrap(),
                30,
                31,
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(replacement.commit_seq, 3);
        assert_eq!(replacement.readiness.epoch, 2);
        let assembly = assemble(&coverage_plan, &replacement.readiness, completion_label);
        (
            coverage_plan,
            CatalogInitialPublicationCommand::new(assembly, replacement.commit_seq, 40, 41),
        )
    }

    fn prepare_refresh(
        connection: &mut Connection,
        initial_label: &[u8],
        refresh_label: &[u8],
    ) -> (
        CatalogCoveragePlan,
        CatalogSnapshotId,
        CatalogRefreshPublicationCommand,
    ) {
        let (coverage_plan, initial) = prepare_building(connection, initial_label);
        let initial_receipt = apply_initial_catalog_publication(connection, &initial)
            .unwrap()
            .unwrap();
        let ready = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        let refresh_started = catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::begin_refresh(ready.refresh_expectation().unwrap(), 40, 41),
        )
        .unwrap()
        .unwrap();
        let active = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        let expected = active.refresh_publication_expectation().unwrap();
        assert_eq!(
            refresh_started.commit_seq,
            expected.refresh_started_commit_seq()
        );
        let reducer = expected.resume_reducer();
        let assembly = CatalogRefreshPublicationAssembly::assemble(
            &coverage_plan,
            &active.readiness,
            refresh_started.commit_seq,
            expected.predecessor().unwrap(),
            expected.prior_reducer(),
            expected.prior_member_history(),
            selection(),
            vec![complete_source(refresh_label).unwrap()],
            &reducer,
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        (
            coverage_plan,
            initial_receipt.snapshot_id,
            CatalogRefreshPublicationCommand::new(assembly, expected, 50, 51),
        )
    }

    fn prepare_source_free_ready(
        connection: &mut Connection,
    ) -> (CatalogCoveragePlan, CatalogSnapshotId) {
        let coverage_plan =
            CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new())
                .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        let pending = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        let scheduled = catalog_state::apply_catalog_build_state_commit(
            connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let assembly = CatalogInitialPublicationAssembly::assemble(
            &coverage_plan,
            &scheduled.readiness,
            selection(),
            Vec::new(),
            &CatalogReducer::default(),
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        let receipt = apply_initial_catalog_publication(
            connection,
            &CatalogInitialPublicationCommand::new(assembly, scheduled.commit_seq, 30, 31),
        )
        .unwrap()
        .unwrap();
        (coverage_plan, receipt.snapshot_id)
    }

    fn count(connection: &Connection, table: &str) -> i64 {
        let sql = match table {
            "ingest_commits" => "SELECT COUNT(*) FROM ingest_commits",
            "catalog_snapshots" => "SELECT COUNT(*) FROM catalog_snapshots",
            "catalog_snapshot_entries" => "SELECT COUNT(*) FROM catalog_snapshot_entries",
            "catalog_build_state" => "SELECT COUNT(*) FROM catalog_build_state",
            "change_log" => "SELECT COUNT(*) FROM change_log",
            _ => unreachable!(),
        };
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn assert_building_unchanged(connection: &Connection) {
        assert_eq!(count(connection, "ingest_commits"), 2);
        assert_eq!(count(connection, "catalog_snapshots"), 0);
        assert_eq!(count(connection, "catalog_snapshot_entries"), 0);
        assert_eq!(count(connection, "catalog_build_state"), 1);
        assert_eq!(count(connection, "change_log"), 2);
        let retained = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        assert_eq!(retained.last_commit_seq, 2);
        assert_eq!(retained.readiness.state, CatalogReadinessPhase::Building);
    }

    fn assert_refreshing_unchanged(
        connection: &Connection,
        predecessor_snapshot: CatalogSnapshotId,
    ) {
        assert_eq!(count(connection, "ingest_commits"), 4);
        assert_eq!(count(connection, "catalog_snapshots"), 1);
        assert_eq!(count(connection, "catalog_snapshot_entries"), 2);
        assert_eq!(count(connection, "catalog_build_state"), 1);
        assert_eq!(count(connection, "change_log"), 4);
        let retained = catalog_state::load_catalog_build_state(connection)
            .unwrap()
            .unwrap();
        assert_eq!(retained.last_commit_seq, 4);
        assert_eq!(retained.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(
            retained.readiness.refreshing_from_snapshot,
            Some(predecessor_snapshot)
        );
        assert_eq!(
            retained.readiness.last_complete_snapshot,
            Some(predecessor_snapshot)
        );
    }

    #[test]
    fn initial_library_publication_is_atomic_ready_and_privacy_safe() {
        let mut connection = database();
        let (coverage_plan, command) = prepare_building(&mut connection, b"completion-a");
        let receipt = apply_initial_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.commit_seq, 3);
        assert_eq!(receipt.snapshot_id.complete_commit, 3);
        assert_eq!(
            receipt.snapshot_id.coverage_plan_id,
            coverage_plan.coverage_plan_id
        );
        assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(receipt.readiness.complete_through_commit, Some(3));
        assert_eq!(
            receipt.readiness.last_complete_snapshot,
            Some(receipt.snapshot_id)
        );
        assert_eq!(count(&connection, "ingest_commits"), 3);
        assert_eq!(count(&connection, "catalog_snapshots"), 1);
        assert_eq!(count(&connection, "catalog_snapshot_entries"), 2);
        assert_eq!(count(&connection, "change_log"), 3);

        let retained = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(retained.readiness, receipt.readiness);
        assert_eq!(retained.last_commit_seq, receipt.commit_seq);

        let payload: Vec<u8> = connection
            .query_row(
                "SELECT payload FROM change_log WHERE commit_seq = 3 AND ordinal = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload_text = String::from_utf8(payload).unwrap();
        assert!(!payload_text.contains("fixture-agent"));
        assert!(!payload_text.contains("private-source-instance"));
        assert!(!payload_text.contains("private-library-policy"));
        assert!(!payload_text.contains("catalog-session-identity-v1"));
        let payload: serde_json::Value = serde_json::from_str(&payload_text).unwrap();
        assert_eq!(payload["state"], "ready");
        assert_eq!(payload["commit_seq"], 3);
        assert_eq!(payload["project_row_count"], 0);
        assert_eq!(payload["session_row_count"], 0);
        assert_eq!(payload["tombstone_count"], 0);
    }

    #[test]
    fn cold_source_generation_replacement_publishes_and_restarts_from_exact_invalidation() {
        let mut connection = database();
        let (coverage_plan, command) =
            prepare_cold_replacement(&mut connection, b"cold-replacement");
        let receipt = apply_initial_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.commit_seq, 4);
        assert_eq!(receipt.snapshot_id.readiness_epoch, 2);
        assert_eq!(
            receipt.snapshot_id.coverage_plan_id,
            coverage_plan.coverage_plan_id
        );
        assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(receipt.readiness.epoch, 2);

        let restarted = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(restarted.readiness, receipt.readiness);
        assert_eq!(
            apply_initial_catalog_publication(&mut connection, &command).unwrap(),
            None
        );

        connection
            .execute_batch(
                "DROP TRIGGER catalog_epoch_invalidations_no_delete; \
                 PRAGMA foreign_keys = OFF; \
                 DELETE FROM catalog_epoch_invalidations;",
            )
            .unwrap();
        let error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("missing source-generation invalidation evidence"),
            "{error}"
        );
    }

    #[test]
    fn cold_source_recovery_publishes_initial_snapshot_from_exact_retry_attempt() {
        let mut connection = database();
        let coverage_plan = plan();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        let pending = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let building = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
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
        let degraded = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::retry_terminal_refresh(
                degraded.expectation().unwrap(),
                40,
                41,
            ),
        )
        .unwrap()
        .unwrap();
        let retry = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(retry.readiness.attempt, 2);
        let command = CatalogInitialPublicationCommand::new(
            assemble(&coverage_plan, &retry.readiness, b"source-recovered"),
            retry.last_commit_seq,
            50,
            51,
        );
        let published = apply_initial_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert_eq!(published.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(published.readiness.attempt, 2);
        assert!(published.readiness.reason.is_none());
        let restarted = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(restarted.readiness, published.readiness);
        assert!(restarted.ready_read_authority().is_ok());
    }

    #[test]
    fn temporary_cold_source_retry_publishes_without_relabeling_the_attempt() {
        let mut connection = database();
        let coverage_plan = plan();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        let pending = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let building = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::mark_initial_build_source_retrying(
                building.initial_source_expectation().unwrap(),
                "catalog_source_temporarily_unavailable",
                30,
                31,
            ),
        )
        .unwrap()
        .unwrap();
        let retrying = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let command = CatalogInitialPublicationCommand::new(
            assemble(&coverage_plan, &retrying.readiness, b"source-returned"),
            retrying.last_commit_seq,
            40,
            41,
        );
        let published = apply_initial_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert_eq!(published.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(published.readiness.attempt, 1);
        assert!(published.readiness.reason.is_none());
        let restarted = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(restarted.readiness, published.readiness);
    }

    #[test]
    fn partial_initial_build_publishes_ready_against_its_exact_milestone() {
        let mut connection = database();
        let coverage_plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![plan_source(), second_plan_source()],
            Vec::new(),
        )
        .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        let pending = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let building = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let first_source = complete_source(b"partial-first").unwrap();
        let partial = catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                building.partial_expectation().unwrap(),
                vec![first_source.source_coverage().clone()],
                30,
                31,
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(partial.readiness.state, CatalogReadinessPhase::Partial);
        let restarted = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(restarted.readiness, partial.readiness);

        let second_source = complete_plan_source(second_plan_source(), b"partial-second").unwrap();
        let assembly = CatalogInitialPublicationAssembly::assemble(
            &coverage_plan,
            &restarted.readiness,
            selection(),
            vec![first_source, second_source],
            &CatalogReducer::default(),
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        let receipt = apply_initial_catalog_publication(
            &mut connection,
            &CatalogInitialPublicationCommand::new(assembly, restarted.last_commit_seq, 40, 41),
        )
        .unwrap()
        .unwrap();
        assert_eq!(receipt.commit_seq, 4);
        assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(
            connection
                .query_row(
                    "SELECT build_commit_seq FROM catalog_snapshots WHERE snapshot_commit_seq = 4",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
        assert_eq!(
            catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness,
            receipt.readiness
        );
    }

    #[test]
    fn partial_recovery_retains_prior_reads_and_publishes_one_refresh_successor() {
        let mut connection = database();
        let coverage_plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![plan_source(), second_plan_source()],
            Vec::new(),
        )
        .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        let pending = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let scheduled = catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let initial_sources = vec![
            complete_source(b"recovery-initial-one").unwrap(),
            complete_plan_source(second_plan_source(), b"recovery-initial-two").unwrap(),
        ];
        let initial_assembly = CatalogInitialPublicationAssembly::assemble(
            &coverage_plan,
            &scheduled.readiness,
            selection(),
            initial_sources,
            &CatalogReducer::default(),
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        let initial = apply_initial_catalog_publication(
            &mut connection,
            &CatalogInitialPublicationCommand::new(initial_assembly, scheduled.commit_seq, 30, 31),
        )
        .unwrap()
        .unwrap();
        let ready = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let refresh = catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::begin_refresh(ready.refresh_expectation().unwrap(), 40, 41),
        )
        .unwrap()
        .unwrap();
        let active = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::degrade_active_refresh(
                active.refresh_publication_expectation().unwrap(),
                "source_temporarily_unavailable",
                50,
                51,
            ),
        )
        .unwrap()
        .unwrap();
        let degraded = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let recovery = catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::retry_terminal_refresh(
                degraded.expectation().unwrap(),
                60,
                61,
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(recovery.readiness.state, CatalogReadinessPhase::Building);
        assert!(catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap()
            .ready_read_authority()
            .is_ok());

        let first_successor = complete_source(b"recovery-successor-one").unwrap();
        let second_successor =
            complete_plan_source(second_plan_source(), b"recovery-successor-two").unwrap();
        let mut progress = recovery.readiness.source_coverage.clone();
        let retained = progress
            .iter_mut()
            .find(|coverage| coverage.scope.adapter_id == "fixture-agent")
            .unwrap();
        *retained = first_successor.source_coverage().clone();
        let recovery_state = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let partial = catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::record_partial(
                recovery_state.partial_expectation().unwrap(),
                progress,
                70,
                71,
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(partial.readiness.state, CatalogReadinessPhase::Partial);
        let partial_state = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert!(partial_state.ready_read_authority().is_ok());
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::mark_active_refresh_retrying(
                partial_state.refresh_publication_expectation().unwrap(),
                "source_retry_pending",
                75,
                76,
            ),
        )
        .unwrap()
        .unwrap();
        let retrying = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(retrying.readiness.state, CatalogReadinessPhase::Partial);
        assert!(matches!(
            retrying.readiness.reason.as_ref(),
            Some(crate::catalog_contract::CatalogReadinessReason::SourceRetrying { .. })
        ));
        assert!(retrying.ready_read_authority().is_ok());
        let expected = retrying.refresh_publication_expectation().unwrap();
        let reducer = expected.resume_reducer();
        let refresh_assembly = CatalogRefreshPublicationAssembly::assemble(
            &coverage_plan,
            &retrying.readiness,
            retrying.last_commit_seq,
            expected.predecessor().unwrap(),
            expected.prior_reducer(),
            expected.prior_member_history(),
            selection(),
            vec![first_successor, second_successor],
            &reducer,
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        let successor = apply_refresh_catalog_publication(
            &mut connection,
            &CatalogRefreshPublicationCommand::new(refresh_assembly, expected, 80, 81),
        )
        .unwrap()
        .unwrap();
        assert_eq!(successor.predecessor_snapshot, initial.snapshot_id);
        assert_eq!(successor.snapshot_id.complete_commit, 9);
        assert_eq!(successor.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(refresh.commit_seq, 4);
        assert!(catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap()
            .ready_read_authority()
            .is_ok());
    }

    #[test]
    fn ordinary_refresh_publishes_a_new_ready_snapshot_and_retains_its_predecessor() {
        let mut connection = database();
        let (coverage_plan, predecessor_snapshot, command) =
            prepare_refresh(&mut connection, b"refresh-initial", b"refresh-successor");
        let receipt = apply_refresh_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.commit_seq, 5);
        assert_eq!(receipt.predecessor_snapshot, predecessor_snapshot);
        assert_eq!(receipt.snapshot_id.complete_commit, 5);
        assert_eq!(
            receipt.snapshot_id.coverage_plan_id,
            coverage_plan.coverage_plan_id
        );
        assert_eq!(
            receipt.snapshot_id.readiness_epoch,
            predecessor_snapshot.readiness_epoch
        );
        assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(receipt.readiness.refreshing_from_snapshot, None);
        assert_eq!(
            receipt.readiness.last_complete_snapshot,
            Some(receipt.snapshot_id)
        );
        assert_eq!(count(&connection, "catalog_snapshots"), 2);
        assert_eq!(count(&connection, "catalog_snapshot_entries"), 5);
        assert_eq!(count(&connection, "ingest_commits"), 5);
        assert_eq!(count(&connection, "change_log"), 5);

        let (predecessor, durable_version, replaced_publication, replaced_content): (
            i64,
            i64,
            Vec<u8>,
            Vec<u8>,
        ) = connection
            .query_row(
                r#"
                    SELECT replaces_snapshot_commit_seq,
                           durable_publication_contract_version,
                           replaces_publication_digest,
                           replaces_content_digest
                    FROM catalog_snapshots WHERE snapshot_commit_seq = 5
                    "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(predecessor, 3);
        assert_eq!(durable_version, 2);
        let (old_publication, old_content): (Vec<u8>, Vec<u8>) = connection
            .query_row(
                "SELECT publication_digest, content_digest FROM catalog_snapshots WHERE snapshot_commit_seq = 3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(replaced_publication, old_publication);
        assert_eq!(replaced_content, old_content);
        assert!(connection
            .execute(
                "UPDATE catalog_snapshots SET replaces_publication_digest = ?1 WHERE snapshot_commit_seq = 5",
                ["x".repeat(DIGEST_BYTES)],
            )
            .is_err());
        assert!(connection
            .execute(
                r#"
                UPDATE catalog_snapshots
                SET replaces_snapshot_commit_seq = NULL,
                    replaces_publication_digest = NULL,
                    replaces_content_digest = NULL
                WHERE snapshot_commit_seq = 5
                "#,
                [],
            )
            .is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM catalog_snapshot_entries WHERE snapshot_commit_seq = 3",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );

        let retained = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(retained.readiness, receipt.readiness);
        let authority = retained.ready_read_authority().unwrap();
        assert!(authority.publication_identity().is_refresh());
        assert_eq!(authority.publication_identity().refresh_depth(), 1);
        let payload: serde_json::Value = serde_json::from_slice(
            &connection
                .query_row(
                    "SELECT payload FROM change_log WHERE commit_seq = 5 AND ordinal = 0",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["predecessor_snapshot"]["complete_commit"], 3);
        assert_eq!(payload["snapshot_id"]["complete_commit"], 5);
        let payload_text = payload.to_string();
        assert!(!payload_text.contains("fixture-agent"));
        assert!(!payload_text.contains("private-source-instance"));
        assert!(!payload_text.contains("private-library-policy"));

        connection
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO catalog_snapshots (
                    snapshot_commit_seq, build_commit_seq,
                    durable_publication_contract_version, pack_contract_version,
                    coverage_plan_id, readiness_epoch, attempt,
                    contract_selection_json, member_identity_contract_id,
                    publication_digest, reducer_revision, entries_digest,
                    content_digest, entry_count, encoded_bytes, source_count,
                    member_count, project_row_count, session_row_count,
                    tombstone_count, replaces_snapshot_commit_seq,
                    replaces_publication_digest, replaces_content_digest, published_at
                )
                SELECT 999, build_commit_seq,
                       durable_publication_contract_version, pack_contract_version,
                       coverage_plan_id, readiness_epoch, attempt,
                       contract_selection_json, member_identity_contract_id,
                       publication_digest, reducer_revision, entries_digest,
                       content_digest, entry_count, encoded_bytes, source_count,
                       member_count, project_row_count, session_row_count,
                       tombstone_count, NULL, NULL, NULL, published_at
                FROM catalog_snapshots WHERE snapshot_commit_seq = 3
                "#,
                [],
            )
            .unwrap();
        let orphan_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(orphan_error
            .to_string()
            .contains("exact bounded current ancestor chain"));
    }

    #[test]
    fn refresh_crash_matrix_rolls_back_and_lost_ack_replays_exactly() {
        for stage in PRECOMMIT_STAGES {
            let mut connection = database();
            let (_, predecessor, command) =
                prepare_refresh(&mut connection, b"refresh-crash", b"refresh-crash-next");
            let result = apply_refresh_catalog_publication_with_hook(
                &mut connection,
                &command,
                &FailAt(stage),
            );
            assert!(
                matches!(result, Err(EngineError::InjectedFailure { .. })),
                "{stage:?}"
            );
            assert_refreshing_unchanged(&connection, predecessor);
        }

        let mut connection = database();
        let (_, predecessor, command) = prepare_refresh(
            &mut connection,
            b"refresh-lost-ack",
            b"refresh-lost-ack-next",
        );
        let result = apply_refresh_catalog_publication_with_hook(
            &mut connection,
            &command,
            &FailAt(CatalogPublicationCommitStage::AfterCommit),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(count(&connection, "ingest_commits"), 5);
        assert_eq!(count(&connection, "catalog_snapshots"), 2);
        assert_eq!(
            apply_refresh_catalog_publication(&mut connection, &command).unwrap(),
            None
        );
        assert_eq!(count(&connection, "ingest_commits"), 5);
        let current = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(current.readiness.refreshing_from_snapshot, None);
        assert_eq!(
            current
                .readiness
                .last_complete_snapshot
                .unwrap()
                .complete_commit,
            5
        );
        assert_eq!(predecessor.complete_commit, 3);
    }

    #[test]
    fn separate_connection_retains_the_predecessor_until_refresh_commit() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("catalog-refresh-visibility.db");
        let mut connection = Connection::open(&database_path).unwrap();
        schema::initialize_schema(&connection).unwrap();
        let (_, predecessor, command) = prepare_refresh(
            &mut connection,
            b"refresh-visibility-initial",
            b"refresh-visibility-successor",
        );
        let predecessor_entries = count(&connection, "catalog_snapshot_entries");
        let hook = AssertExternalRefreshing {
            database_path: database_path.clone(),
            predecessor_snapshot: predecessor,
            predecessor_entries,
            observed: Cell::new(false),
        };
        let receipt = apply_refresh_catalog_publication_with_hook(&mut connection, &command, &hook)
            .unwrap()
            .unwrap();
        assert!(hook.observed.get());
        assert_eq!(receipt.commit_seq, 5);
        let external = Connection::open(database_path).unwrap();
        assert_eq!(count(&external, "catalog_snapshots"), 2);
        assert_eq!(count(&external, "catalog_snapshot_entries"), 5);
        let state = catalog_state::load_catalog_build_state(&external)
            .unwrap()
            .unwrap();
        assert_eq!(state.readiness.refreshing_from_snapshot, None);
        assert_eq!(
            state.readiness.last_complete_snapshot,
            Some(receipt.snapshot_id)
        );
    }

    #[test]
    fn retained_refresh_lineage_accepts_its_exact_ceiling_and_refuses_one_more() {
        let mut connection = database();
        let (coverage_plan, _) = prepare_source_free_ready(&mut connection);
        for index in 0..MAX_RETAINED_REFRESH_LINEAGE_DEPTH {
            let ready = catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap();
            let started = catalog_state::apply_catalog_build_state_commit(
                &mut connection,
                &CatalogBuildStateCommand::begin_refresh(
                    ready.refresh_expectation().unwrap(),
                    100 + index as i64 * 4,
                    101 + index as i64 * 4,
                ),
            )
            .unwrap()
            .unwrap();
            let active = catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap();
            let expected = active.refresh_publication_expectation().unwrap();
            let reducer = expected.resume_reducer();
            let assembly = CatalogRefreshPublicationAssembly::assemble(
                &coverage_plan,
                &active.readiness,
                started.commit_seq,
                expected.predecessor().unwrap(),
                expected.prior_reducer(),
                expected.prior_member_history(),
                selection(),
                Vec::new(),
                &reducer,
                Vec::new(),
                CatalogPublicationLimits::default(),
            )
            .unwrap();
            apply_refresh_catalog_publication(
                &mut connection,
                &CatalogRefreshPublicationCommand::new(
                    assembly,
                    expected,
                    102 + index as i64 * 4,
                    103 + index as i64 * 4,
                ),
            )
            .unwrap()
            .unwrap();
        }

        let ready = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let authority = ready.ready_read_authority().unwrap();
        assert_eq!(
            authority.publication_identity().refresh_depth(),
            MAX_RETAINED_REFRESH_LINEAGE_DEPTH
        );
        assert!(ready.refresh_expectation().is_err());
        let retirement = CatalogSnapshotRetirementCommand::new(
            ready.snapshot_retirement_expectation().unwrap(),
            1_000,
            1_001,
        )
        .unwrap();
        crate::engine::catalog_retention::apply_catalog_snapshot_retirement(
            &mut connection,
            &retirement,
        )
        .unwrap()
        .unwrap();
        let retired = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert!(retired.refresh_expectation().is_err());
        assert_eq!(count(&connection, "catalog_snapshots"), 9);
        let Err(simulated_one_over) = load_ready_publication_at_depth(
            &connection,
            &coverage_plan,
            authority.snapshot_id(),
            ready.readiness.attempt,
            1,
        ) else {
            panic!("one-over retained refresh lineage unexpectedly loaded")
        };
        assert!(simulated_one_over
            .to_string()
            .contains("bounded retained depth"));
    }

    #[test]
    fn source_free_plan_publishes_complete_empty_and_restart_rejects_base_reference_drift() {
        let mut connection = database();
        let coverage_plan =
            CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new())
                .unwrap();
        catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::register(coverage_plan.clone(), 1, 10, 11),
        )
        .unwrap()
        .unwrap();
        let pending = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let scheduled = catalog_state::apply_catalog_build_state_commit(
            &mut connection,
            &CatalogBuildStateCommand::schedule(pending.expectation().unwrap(), 20, 21),
        )
        .unwrap()
        .unwrap();
        let assembly = CatalogInitialPublicationAssembly::assemble(
            &coverage_plan,
            &scheduled.readiness,
            selection(),
            Vec::new(),
            &CatalogReducer::default(),
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        let receipt = apply_initial_catalog_publication(
            &mut connection,
            &CatalogInitialPublicationCommand::new(assembly, scheduled.commit_seq, 30, 31),
        )
        .unwrap()
        .unwrap();
        assert_eq!(receipt.readiness.state, CatalogReadinessPhase::Ready);
        assert!(receipt.readiness.source_coverage.is_empty());
        assert_eq!(count(&connection, "catalog_snapshot_entries"), 1);
        let (source_count, identity_contract): (i64, Option<String>) = connection
            .query_row(
                "SELECT source_count, member_identity_contract_id FROM catalog_snapshots",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(source_count, 0);
        assert_eq!(identity_contract, None);
        assert_eq!(
            catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness,
            receipt.readiness
        );
        let original_selection: Vec<u8> = connection
            .query_row(
                "SELECT contract_selection_json FROM catalog_snapshots",
                [],
                |row| row.get(0),
            )
            .unwrap();
        for field in [
            "model_major",
            "external_entity_reference_version",
            "semantic_revision_reference_version",
        ] {
            let mut drifted: serde_json::Value =
                serde_json::from_slice(&original_selection).unwrap();
            drifted[field] = serde_json::json!(2);
            connection
                .execute(
                    "UPDATE catalog_snapshots SET contract_selection_json = ?1",
                    [serde_json::to_vec(&drifted).unwrap()],
                )
                .unwrap();
            let error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
            assert!(error
                .to_string()
                .contains("exact RFC 012A/B contract selection"));
        }
        connection
            .execute(
                "UPDATE catalog_snapshots SET contract_selection_json = ?1",
                [original_selection],
            )
            .unwrap();
    }

    #[test]
    fn exact_replay_is_noop_but_stale_or_drifted_publication_fails_closed() {
        let mut connection = database();
        let (coverage_plan, command) = prepare_building(&mut connection, b"completion-a");
        apply_initial_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert_eq!(
            apply_initial_catalog_publication(&mut connection, &command).unwrap(),
            None
        );
        assert_eq!(count(&connection, "ingest_commits"), 3);
        assert_eq!(count(&connection, "catalog_snapshots"), 1);
        assert_eq!(count(&connection, "catalog_snapshot_entries"), 2);
        assert_eq!(count(&connection, "change_log"), 3);

        let ready = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        let drifted = CatalogInitialPublicationCommand::new(
            assemble(
                &coverage_plan,
                &building_snapshot(&coverage_plan),
                b"completion-b",
            ),
            2,
            30,
            31,
        );
        assert!(apply_initial_catalog_publication(&mut connection, &drifted).is_err());
        let stale = CatalogInitialPublicationCommand::new(
            assemble(
                &coverage_plan,
                &building_snapshot(&coverage_plan),
                b"completion-a",
            ),
            1,
            30,
            31,
        );
        assert!(apply_initial_catalog_publication(&mut connection, &stale).is_err());
        assert_eq!(ready.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(count(&connection, "ingest_commits"), 3);
    }

    #[test]
    fn building_commit_compare_and_swap_is_exact_before_any_snapshot_write() {
        for stale_commit in [1, 3] {
            let mut connection = database();
            let (coverage_plan, _) = prepare_building(&mut connection, b"exact-cas");
            let command = CatalogInitialPublicationCommand::new(
                assemble(
                    &coverage_plan,
                    &building_snapshot(&coverage_plan),
                    b"exact-cas",
                ),
                stale_commit,
                30,
                31,
            );
            assert!(apply_initial_catalog_publication(&mut connection, &command).is_err());
            assert_building_unchanged(&connection);
        }
    }

    fn building_snapshot(plan: &CatalogCoveragePlan) -> CatalogReadinessSnapshot {
        let mut machine =
            CatalogReadinessMachine::register(plan.clone(), CATALOG_QUERY_PACK_CONTRACT_VERSION)
                .unwrap();
        machine.schedule_build().unwrap();
        machine.snapshot().clone()
    }

    #[test]
    fn every_precommit_failure_rolls_back_and_lost_ack_replays_idempotently() {
        for stage in PRECOMMIT_STAGES {
            let mut connection = database();
            let (coverage_plan, _) = prepare_building(&mut connection, b"crash-matrix");
            let command = CatalogInitialPublicationCommand::new(
                rich_assembly(&coverage_plan, &building_snapshot(&coverage_plan)),
                2,
                30,
                31,
            );
            let result = apply_initial_catalog_publication_with_hook(
                &mut connection,
                &command,
                &FailAt(stage),
            );
            assert!(
                matches!(result, Err(EngineError::InjectedFailure { .. })),
                "{stage:?}"
            );
            assert_building_unchanged(&connection);
        }

        let mut connection = database();
        let (coverage_plan, _) = prepare_building(&mut connection, b"lost-ack");
        let command = CatalogInitialPublicationCommand::new(
            rich_assembly(&coverage_plan, &building_snapshot(&coverage_plan)),
            2,
            30,
            31,
        );
        let result = apply_initial_catalog_publication_with_hook(
            &mut connection,
            &command,
            &FailAt(CatalogPublicationCommitStage::AfterCommit),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        assert_eq!(count(&connection, "ingest_commits"), 3);
        assert_eq!(count(&connection, "catalog_snapshots"), 1);
        assert_eq!(count(&connection, "catalog_snapshot_entries"), 6);
        assert_eq!(count(&connection, "change_log"), 3);
        assert_eq!(
            apply_initial_catalog_publication(&mut connection, &command).unwrap(),
            None
        );
        assert_eq!(count(&connection, "ingest_commits"), 3);
    }

    #[test]
    fn separate_connection_sees_building_until_the_complete_publication_commits() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("catalog-visibility.db");
        let mut connection = Connection::open(&database_path).unwrap();
        schema::initialize_schema(&connection).unwrap();
        let (coverage_plan, _) = prepare_building(&mut connection, b"visibility");
        let command = CatalogInitialPublicationCommand::new(
            rich_assembly(&coverage_plan, &building_snapshot(&coverage_plan)),
            2,
            30,
            31,
        );
        let hook = AssertExternalBuilding {
            database_path: database_path.clone(),
            target: CatalogPublicationCommitStage::AfterReadinessWrite,
            observed: Cell::new(false),
        };
        let receipt = apply_initial_catalog_publication_with_hook(&mut connection, &command, &hook)
            .unwrap()
            .unwrap();
        assert!(hook.observed.get());
        assert_eq!(receipt.commit_seq, 3);
        let external = Connection::open(database_path).unwrap();
        let (state, snapshots, entries): (String, i64, i64) = external
            .query_row(
                r#"
                SELECT state,
                       (SELECT COUNT(*) FROM catalog_snapshots),
                       (SELECT COUNT(*) FROM catalog_snapshot_entries)
                FROM catalog_build_state WHERE scope_kind = 'library'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, "ready");
        assert_eq!(snapshots, 1);
        assert_eq!(entries, 6);
    }

    #[test]
    fn restart_reconstructs_ready_and_rejects_durable_frame_corruption() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("catalog-publication.db");
        {
            let mut connection = Connection::open(&database_path).unwrap();
            schema::initialize_schema(&connection).unwrap();
            let (_, command) = prepare_building(&mut connection, b"restart");
            apply_initial_catalog_publication(&mut connection, &command)
                .unwrap()
                .unwrap();
        }
        {
            let connection = Connection::open(&database_path).unwrap();
            schema::initialize_schema(&connection).unwrap();
            let ready = catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap();
            assert_eq!(ready.readiness.state, CatalogReadinessPhase::Ready);
            assert_eq!(ready.readiness.complete_through_commit, Some(3));
        }
        let mut runtime = WriterRuntime::start(database_path.clone()).unwrap();
        runtime.shutdown().unwrap();

        let connection = Connection::open(&database_path).unwrap();
        let original_encoded_bytes: i64 = connection
            .query_row("SELECT encoded_bytes FROM catalog_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        connection
            .execute("UPDATE catalog_snapshots SET encoded_bytes = 1", [])
            .unwrap();
        let payload_bound_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(payload_bound_error
            .to_string()
            .contains("entry payload is outside its durable byte bound"));
        connection
            .execute(
                "UPDATE catalog_snapshots SET encoded_bytes = ?1",
                [original_encoded_bytes],
            )
            .unwrap();

        let original_payload: Vec<u8> = connection
            .query_row(
                "SELECT payload FROM catalog_snapshot_entries WHERE ordinal = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET payload = ?1 WHERE ordinal = 0",
                ["{}"],
            )
            .unwrap();
        let payload_type_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(payload_type_error
            .to_string()
            .contains("entry payload is outside its durable byte bound"));
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET payload = ?1 WHERE ordinal = 0",
                [original_payload],
            )
            .unwrap();

        let original_payload_digest: Vec<u8> = connection
            .query_row(
                "SELECT payload_digest FROM catalog_snapshot_entries WHERE ordinal = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET payload_digest = ?1 WHERE ordinal = 0",
                ["x".repeat(DIGEST_BYTES)],
            )
            .unwrap();
        let digest_type_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(digest_type_error
            .to_string()
            .contains("payload digest exceeds its fixed durable bound"));
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET payload_digest = ?1 WHERE ordinal = 0",
                [original_payload_digest],
            )
            .unwrap();

        let original_reducer_key: Vec<u8> = connection
            .query_row(
                "SELECT entry_key FROM catalog_snapshot_entries WHERE entry_kind = 'reducer_state'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET entry_key = ?1 WHERE entry_kind = 'reducer_state'",
                ["x".repeat(DIGEST_BYTES)],
            )
            .unwrap();
        let key_type_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(key_type_error
            .to_string()
            .contains("entry key exceeds its fixed durable bound"));
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET entry_key = ?1 WHERE entry_kind = 'reducer_state'",
                [&original_reducer_key],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET entry_key = ?1 WHERE entry_kind = 'reducer_state'",
                [vec![7_u8; DIGEST_BYTES]],
            )
            .unwrap();
        let reducer_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(reducer_error
            .to_string()
            .contains("reducer-state frame key"));
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET entry_key = ?1 WHERE entry_kind = 'reducer_state'",
                [original_reducer_key],
            )
            .unwrap();
        assert_eq!(
            catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness
                .state,
            CatalogReadinessPhase::Ready
        );
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET payload = x'7b7d' WHERE ordinal = 0",
                [],
            )
            .unwrap();
        assert!(catalog_state::load_catalog_build_state(&connection).is_err());
        match WriterRuntime::start(database_path) {
            Err(EngineError::InvalidCommit(message)) => {
                assert!(message.contains("payload digest"));
            }
            Err(other) => panic!("unexpected writer restart error: {other}"),
            Ok(mut runtime) => {
                runtime.shutdown().unwrap();
                panic!("corrupt catalog snapshot unexpectedly started the writer");
            }
        }
    }

    #[test]
    fn restart_rejects_oversized_header_cells_before_rust_decoding() {
        let mut connection = database();
        let (_, command) = prepare_building(&mut connection, b"bounded-header");
        apply_initial_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();

        for column in [
            "coverage_plan_id",
            "publication_digest",
            "reducer_revision",
            "entries_digest",
            "content_digest",
        ] {
            let select = format!("SELECT {column} FROM catalog_snapshots");
            let original: Vec<u8> = connection.query_row(&select, [], |row| row.get(0)).unwrap();
            let update = format!("UPDATE catalog_snapshots SET {column} = ?1");
            connection.execute(&update, [vec![7_u8; 33]]).unwrap();
            let error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
            assert!(
                error.to_string().contains("fixed durable bound"),
                "{column}: {error}"
            );
            connection.execute(&update, [original]).unwrap();
        }

        let original_identity: String = connection
            .query_row(
                "SELECT member_identity_contract_id FROM catalog_snapshots",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_snapshots SET member_identity_contract_id = ?1",
                ["x".repeat(257)],
            )
            .unwrap();
        let error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(error
            .to_string()
            .contains("member identity contract exceeds its durable byte bound"));
        connection
            .execute(
                "UPDATE catalog_snapshots SET member_identity_contract_id = ?1",
                [original_identity],
            )
            .unwrap();
        assert_eq!(
            catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness
                .state,
            CatalogReadinessPhase::Ready
        );
    }

    #[test]
    fn refresh_restart_rejects_predecessor_history_and_owner_corruption() {
        let mut connection = database();
        let (_, _, command) = prepare_refresh(
            &mut connection,
            b"refresh-restart-initial",
            b"refresh-restart-successor",
        );
        apply_refresh_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert_eq!(
            catalog_state::load_catalog_build_state(&connection)
                .unwrap()
                .unwrap()
                .readiness
                .last_complete_snapshot
                .unwrap()
                .complete_commit,
            5
        );
        connection
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;")
            .unwrap();

        let predecessor_digest: Vec<u8> = connection
            .query_row(
                "SELECT replaces_publication_digest FROM catalog_snapshots WHERE snapshot_commit_seq = 5",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE catalog_snapshots SET replaces_publication_digest = ?1 WHERE snapshot_commit_seq = 5",
                ["x".repeat(DIGEST_BYTES)],
            )
            .unwrap();
        assert!(catalog_state::load_catalog_build_state(&connection).is_err());
        connection
            .execute(
                "UPDATE catalog_snapshots SET replaces_publication_digest = ?1 WHERE snapshot_commit_seq = 5",
                [&predecessor_digest],
            )
            .unwrap();

        let (history_payload, history_digest): (Vec<u8>, Vec<u8>) = connection
            .query_row(
                "SELECT payload, payload_digest FROM catalog_snapshot_entries WHERE snapshot_commit_seq = 5 AND entry_kind = 'member_history'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let invalid_history = b"{}".to_vec();
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET payload = ?1, payload_digest = ?2 WHERE snapshot_commit_seq = 5 AND entry_kind = 'member_history'",
                params![invalid_history, blake3::hash(b"{}").as_bytes().as_slice()],
            )
            .unwrap();
        let history_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(history_error.to_string().contains("member history"));
        connection
            .execute(
                "UPDATE catalog_snapshot_entries SET payload = ?1, payload_digest = ?2 WHERE snapshot_commit_seq = 5 AND entry_kind = 'member_history'",
                params![history_payload, history_digest],
            )
            .unwrap();

        connection
            .execute(
                "UPDATE catalog_snapshots SET readiness_epoch = 2 WHERE snapshot_commit_seq = 3",
                [],
            )
            .unwrap();
        let lineage_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(lineage_error
            .to_string()
            .contains("exact plan/build lineage"));
        connection
            .execute(
                "UPDATE catalog_snapshots SET readiness_epoch = 1 WHERE snapshot_commit_seq = 3",
                [],
            )
            .unwrap();

        connection
            .execute(
                "UPDATE ingest_commits SET reason = ?1 WHERE commit_seq = 5",
                [catalog_state::INITIAL_PUBLICATION_REASON],
            )
            .unwrap();
        let owner_error = catalog_state::load_catalog_build_state(&connection).unwrap_err();
        assert!(owner_error
            .to_string()
            .contains("expected source-neutral commit"));
    }

    #[test]
    fn schema_rejects_forged_ready_linkage_and_restart_rejects_snapshot_coordinate_drift() {
        let mut connection = database();
        let (_, command) = prepare_building(&mut connection, b"schema-linkage");
        assert!(connection
            .execute(
                r#"
                UPDATE catalog_build_state
                SET state = 'ready', completed_contract_version = 1,
                    complete_through_commit = last_commit_seq,
                    last_complete_snapshot_commit = last_commit_seq
                WHERE scope_kind = 'library'
                "#,
                [],
            )
            .is_err());
        apply_initial_catalog_publication(&mut connection, &command)
            .unwrap()
            .unwrap();
        assert!(connection
            .execute("UPDATE catalog_snapshots SET readiness_epoch = 2", [],)
            .is_ok());
        assert!(catalog_state::load_catalog_build_state(&connection).is_err());
    }

    #[test]
    fn writer_command_path_publishes_one_commit_and_suppresses_replay() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("catalog-writer.db");
        let mut runtime = WriterRuntime::start(database_path.clone()).unwrap();
        let writer = runtime.client();
        let coverage_plan = plan();
        let registered = writer
            .commit_catalog_build_state(CatalogBuildStateCommand::register(
                coverage_plan.clone(),
                1,
                10,
                11,
            ))
            .unwrap()
            .unwrap();
        let scheduled = writer
            .commit_catalog_build_state(CatalogBuildStateCommand::schedule(
                catalog_state::CatalogBuildExpectation {
                    scope: CatalogCoverageScope::Library,
                    coverage_plan_id: coverage_plan.coverage_plan_id,
                    desired_contract_version: 1,
                    epoch: 1,
                    attempt: 1,
                    state: catalog_state::CatalogDurableBuildPhase::Pending,
                    state_commit_seq: 1,
                },
                20,
                21,
            ))
            .unwrap()
            .unwrap();
        assert_eq!(registered.commit_seq, 1);
        assert_eq!(scheduled.commit_seq, 2);
        let command = CatalogInitialPublicationCommand::new(
            assemble(&coverage_plan, &scheduled.readiness, b"writer"),
            scheduled.commit_seq,
            30,
            31,
        );
        assert_eq!(
            writer
                .commit_initial_catalog_publication(command.clone())
                .unwrap()
                .unwrap()
                .commit_seq,
            3
        );
        assert_eq!(
            writer.commit_initial_catalog_publication(command).unwrap(),
            None
        );
        let reader = Connection::open(&database_path).unwrap();
        let ready = catalog_state::load_catalog_build_state(&reader)
            .unwrap()
            .unwrap();
        assert_eq!(
            writer
                .commit_catalog_build_state(CatalogBuildStateCommand::begin_refresh(
                    ready.refresh_expectation().unwrap(),
                    40,
                    41,
                ))
                .unwrap()
                .unwrap()
                .commit_seq,
            4
        );
        let active = catalog_state::load_catalog_build_state(&reader)
            .unwrap()
            .unwrap();
        let expected = active.refresh_publication_expectation().unwrap();
        let reducer = expected.resume_reducer();
        let refresh = CatalogRefreshPublicationAssembly::assemble(
            &coverage_plan,
            &active.readiness,
            expected.refresh_started_commit_seq(),
            expected.predecessor().unwrap(),
            expected.prior_reducer(),
            expected.prior_member_history(),
            selection(),
            vec![complete_source(b"writer-refresh").unwrap()],
            &reducer,
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        let refresh = CatalogRefreshPublicationCommand::new(refresh, expected, 50, 51);
        assert_eq!(
            writer
                .commit_refresh_catalog_publication(refresh.clone())
                .unwrap()
                .unwrap()
                .commit_seq,
            5
        );
        assert_eq!(
            writer.commit_refresh_catalog_publication(refresh).unwrap(),
            None
        );
        let ready = catalog_state::load_catalog_build_state(&reader)
            .unwrap()
            .unwrap();
        let retirement = CatalogSnapshotRetirementCommand::new(
            ready.snapshot_retirement_expectation().unwrap(),
            60,
            61,
        )
        .unwrap();
        assert_eq!(
            writer
                .retire_catalog_snapshot(retirement.clone())
                .unwrap()
                .unwrap()
                .commit_seq,
            6
        );
        assert_eq!(writer.retire_catalog_snapshot(retirement).unwrap(), None);
        runtime.shutdown().unwrap();
        let connection = Connection::open(database_path).unwrap();
        let ready = catalog_state::load_catalog_build_state(&connection)
            .unwrap()
            .unwrap();
        assert_eq!(ready.readiness.state, CatalogReadinessPhase::Ready);
        assert_eq!(
            ready
                .readiness
                .last_complete_snapshot
                .unwrap()
                .complete_commit,
            5
        );
    }

    #[test]
    fn engine_publishes_only_the_new_ready_commit_watermark() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("catalog-core.db");
        let engine = SpaghettiEngineCore::open(EngineOptions {
            database_path: database_path.clone(),
            query_workers: Some(1),
            owner_label: Some("catalog-publication-test".to_owned()),
            defer_query_structures: false,
            source_pass_pool: None,
        })
        .unwrap();
        let coverage_plan = plan();
        assert_eq!(
            engine
                .commit_catalog_build_state(CatalogBuildStateCommand::register(
                    coverage_plan.clone(),
                    1,
                    10,
                    11,
                ))
                .unwrap(),
            Some(1)
        );
        let pending = catalog_state::CatalogBuildExpectation {
            scope: CatalogCoverageScope::Library,
            coverage_plan_id: coverage_plan.coverage_plan_id,
            desired_contract_version: 1,
            epoch: 1,
            attempt: 1,
            state: catalog_state::CatalogDurableBuildPhase::Pending,
            state_commit_seq: 1,
        };
        assert_eq!(
            engine
                .commit_catalog_build_state(CatalogBuildStateCommand::schedule(pending, 20, 21,))
                .unwrap(),
            Some(2)
        );
        let command = CatalogInitialPublicationCommand::new(
            assemble(
                &coverage_plan,
                &building_snapshot(&coverage_plan),
                b"engine-core",
            ),
            2,
            30,
            31,
        );
        assert_eq!(engine.latest_commit_seq(), 2);
        assert_eq!(
            engine
                .commit_initial_catalog_publication(command.clone())
                .unwrap()
                .unwrap()
                .commit_seq,
            3
        );
        assert_eq!(engine.latest_commit_seq(), 3);
        assert_eq!(
            engine.commit_initial_catalog_publication(command).unwrap(),
            None
        );
        assert_eq!(engine.latest_commit_seq(), 3);

        let reader = Connection::open(&database_path).unwrap();
        let ready = catalog_state::load_catalog_build_state(&reader)
            .unwrap()
            .unwrap();
        assert_eq!(
            engine
                .commit_catalog_build_state(CatalogBuildStateCommand::begin_refresh(
                    ready.refresh_expectation().unwrap(),
                    40,
                    41,
                ))
                .unwrap(),
            Some(4)
        );
        assert_eq!(engine.latest_commit_seq(), 4);
        let active = catalog_state::load_catalog_build_state(&reader)
            .unwrap()
            .unwrap();
        let expected = active.refresh_publication_expectation().unwrap();
        let reducer = expected.resume_reducer();
        let refresh = CatalogRefreshPublicationAssembly::assemble(
            &coverage_plan,
            &active.readiness,
            expected.refresh_started_commit_seq(),
            expected.predecessor().unwrap(),
            expected.prior_reducer(),
            expected.prior_member_history(),
            selection(),
            vec![complete_source(b"engine-refresh").unwrap()],
            &reducer,
            Vec::new(),
            CatalogPublicationLimits::default(),
        )
        .unwrap();
        let refresh = CatalogRefreshPublicationCommand::new(refresh, expected, 50, 51);
        assert_eq!(
            engine
                .commit_refresh_catalog_publication(refresh.clone())
                .unwrap()
                .unwrap()
                .commit_seq,
            5
        );
        assert_eq!(engine.latest_commit_seq(), 5);
        assert_eq!(
            engine.commit_refresh_catalog_publication(refresh).unwrap(),
            None
        );
        assert_eq!(engine.latest_commit_seq(), 5);
        let ready = catalog_state::load_catalog_build_state(&reader)
            .unwrap()
            .unwrap();
        let retirement = CatalogSnapshotRetirementCommand::new(
            ready.snapshot_retirement_expectation().unwrap(),
            60,
            61,
        )
        .unwrap();
        assert_eq!(
            engine
                .retire_catalog_snapshot(retirement.clone())
                .unwrap()
                .unwrap()
                .commit_seq,
            6
        );
        assert_eq!(engine.latest_commit_seq(), 6);
        assert_eq!(engine.retire_catalog_snapshot(retirement).unwrap(), None);
        assert_eq!(engine.latest_commit_seq(), 6);
        engine.shutdown().unwrap();
    }

    #[test]
    fn publication_command_debug_redacts_private_source_material() {
        let mut connection = database();
        let (_, command) = prepare_building(&mut connection, b"private-completion");
        let debug = format!("{command:?}");
        assert!(!debug.contains("private-source-instance"));
        assert!(!debug.contains("private-library-policy"));
        assert!(!debug.contains("private-completion"));
        let durable_debug = format!("{:?}", command.assembly.prepare_durable().unwrap());
        assert!(!durable_debug.contains("private-source-instance"));
        assert!(!durable_debug.contains("private-library-policy"));
        assert!(!durable_debug.contains("private-completion"));

        let mut refresh_connection = database();
        let (_, _, refresh) = prepare_refresh(
            &mut refresh_connection,
            b"private-refresh-initial",
            b"private-refresh-completion",
        );
        let refresh_debug = format!("{refresh:?}");
        assert!(!refresh_debug.contains("private-source-instance"));
        assert!(!refresh_debug.contains("private-library-policy"));
        assert!(!refresh_debug.contains("private-refresh-completion"));
        let durable_debug = format!("{:?}", refresh.assembly.prepare_durable().unwrap());
        assert!(!durable_debug.contains("private-source-instance"));
        assert!(!durable_debug.contains("private-library-policy"));
        assert!(!durable_debug.contains("private-refresh-completion"));
    }

    #[test]
    fn durable_aggregate_preflight_rejects_low_limits_before_any_sql() {
        let mut connection = database();
        let (_, command) = prepare_building(&mut connection, b"bounded-preflight");
        let durable = command.assembly.prepare_durable().unwrap();
        let first_source = durable
            .entries()
            .iter()
            .find(|entry| entry.kind() == CatalogDurablePublicationEntryKind::Source)
            .unwrap();
        let bytes_through_first_source = durable.contract_selection_json().len()
            + durable.member_identity_contract_id().map_or(0, str::len)
            + first_source.payload().len()
            + DIGEST_BYTES * 2
            + first_source.kind().as_str().len();

        let entry_error = command
            .assembly
            .prepare_durable_with_test_limits(1, MAX_DURABLE_PUBLICATION_BYTES)
            .unwrap_err();
        assert!(entry_error.to_string().contains("reducer_state"));
        let byte_error = command
            .assembly
            .prepare_durable_with_test_limits(
                MAX_DURABLE_PUBLICATION_ENTRIES,
                bytes_through_first_source,
            )
            .unwrap_err();
        assert!(byte_error.to_string().contains("reducer_state"));
        assert_building_unchanged(&connection);

        let mut refresh_connection = database();
        let (_, predecessor, refresh) = prepare_refresh(
            &mut refresh_connection,
            b"bounded-refresh-initial",
            b"bounded-refresh-successor",
        );
        let entry_error = refresh
            .assembly
            .prepare_durable_with_test_limits(1, MAX_DURABLE_PUBLICATION_BYTES)
            .unwrap_err();
        assert!(entry_error.to_string().contains("member_history"));
        let durable = refresh.assembly.prepare_durable().unwrap();
        let first_source = durable
            .entries()
            .iter()
            .find(|entry| entry.kind() == CatalogDurablePublicationEntryKind::Source)
            .unwrap();
        let bytes_through_first_source = durable.contract_selection_json().len()
            + durable.member_identity_contract_id().map_or(0, str::len)
            + first_source.payload().len()
            + DIGEST_BYTES * 2
            + first_source.kind().as_str().len();
        let byte_error = refresh
            .assembly
            .prepare_durable_with_test_limits(
                MAX_DURABLE_PUBLICATION_ENTRIES,
                bytes_through_first_source,
            )
            .unwrap_err();
        assert!(byte_error.to_string().contains("member_history"));
        assert_refreshing_unchanged(&refresh_connection, predecessor);
    }
}
