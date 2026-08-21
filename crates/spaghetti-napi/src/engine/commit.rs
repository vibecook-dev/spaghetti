//! RFC 011 atomic source-cursor commit coordinator.
//!
//! This module owns the transaction boundary shared by catalog state,
//! projection readiness, record diagnostics, and the durable change log. The
//! adapter-specific fact/projector implementations land in later phases and
//! must execute inside this boundary before the cursor is advanced.

use std::collections::BTreeSet;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::adapter::{ConsistencyPolicy, RawRetentionPolicy};

use super::source_coverage::{self, DurableCoverageSetUpdate};
use super::EngineError;

const MAX_DRIVER_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROJECTION_VERSION_UPDATES: usize = 64;
const MAX_QUERY_PACK_SELECTION_UPDATES: usize = 16;
const MAX_PROJECTION_SCOPE_KEY_BYTES: usize = 4 * 1024;
const MAX_QUERY_PACK_ID_BYTES: usize = 256;
const CHANGE_LOG_ROW_OVERHEAD_BYTES: u64 = 24;

/// Default durable replay window. Size is measured as the encoded change
/// fields plus a fixed cursor/schema overhead, rather than SQLite page usage,
/// so the bound remains deterministic across SQLite builds and page sizes.
pub const DEFAULT_CHANGE_LOG_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
pub const DEFAULT_CHANGE_LOG_MAX_PAYLOAD_BYTES: u64 = 128 * 1024 * 1024;
// Backfill commits are intentionally much wider than live commits. Keeping
// 1,024 of those groups could pin several gigabytes despite the 128 MiB size
// target; 128 still protects a substantial cursor window, while age/size keep
// ordinary low-volume live history considerably longer.
pub const DEFAULT_CHANGE_LOG_MIN_RESUMABLE_COMMITS: u64 = 128;
// Retention is a bounded housekeeping task, not part of every logical commit.
// Running it once per small commit group keeps age/size overshoot bounded to
// at most 31 additional commits while avoiding repeated aggregate/window
// queries that cannot prune anything inside the protected 128-commit window.
const AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS: u64 = 32;
const AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_MS: i64 = 5 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeLogRetentionPolicy {
    pub max_age_ms: u64,
    pub max_payload_bytes: u64,
    pub min_resumable_commits: u64,
}

impl Default for ChangeLogRetentionPolicy {
    fn default() -> Self {
        Self {
            max_age_ms: DEFAULT_CHANGE_LOG_MAX_AGE_MS,
            max_payload_bytes: DEFAULT_CHANGE_LOG_MAX_PAYLOAD_BYTES,
            min_resumable_commits: DEFAULT_CHANGE_LOG_MIN_RESUMABLE_COMMITS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeLogRetentionSnapshot {
    pub pruned_through_commit_seq: u64,
    pub retained_change_count: u64,
    pub retained_payload_bytes: u64,
    pub oldest_retained_commit_seq: Option<u64>,
    pub oldest_retained_ordinal: Option<u32>,
}

/// Stable schema version for a change payload. This is independent from the
/// SQLite schema version and is supplied by the projector that owns a topic.
pub type ChangeSchemaVersion = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInstanceSpec {
    pub adapter_id: String,
    pub stable_key: Vec<u8>,
    pub display_name: String,
    pub adapter_version: String,
    pub adapter_contract_version: u32,
    pub source_schema_versions: Vec<String>,
    pub capabilities: Vec<SourceCapabilitySpec>,
    pub discovered_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCapabilitySpec {
    pub id: String,
    pub support_level: String,
    pub granularity: String,
    pub availability: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStreamSpec {
    pub stream_key: String,
    pub driver_kind: String,
    pub decoder_key: String,
    pub stream_state: String,
    pub last_reconciled_at: Option<i64>,
    pub consistency: ConsistencyPolicy,
    pub retention: RawRetentionPolicy,
}

/// Compare-and-swap precondition for a source-object update. `Absent` is not
/// interchangeable with an empty cursor: it means the catalog row must not
/// exist yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedSourceCursor {
    Absent,
    At {
        generation: u64,
        committed_cursor: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceObjectUpdate {
    pub object_key: Vec<u8>,
    pub expected: ExpectedSourceCursor,
    pub display_path: Option<String>,
    pub native_identity: Option<Vec<u8>>,
    pub generation: u64,
    pub committed_cursor: Vec<u8>,
    pub observed_revision: Option<Vec<u8>>,
    pub adapter_object_context: Option<Vec<u8>>,
    pub driver_checkpoint: Option<Vec<u8>>,
    pub driver_checkpoint_version: Option<u32>,
    pub decoder_state: Option<Vec<u8>>,
    pub decoder_state_version: Option<u32>,
    pub retry_state: Option<Vec<u8>>,
    pub size_bytes: Option<u64>,
    pub mtime_ns: Option<i64>,
    pub decoder_contract_version: u32,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionReadiness {
    Ready,
    StaleSafe,
    Pending,
    Unavailable,
}

impl ProjectionReadiness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::StaleSafe => "stale_safe",
            Self::Pending => "pending",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionVersionUpdate {
    pub projection_id: String,
    pub scope_key: Vec<u8>,
    pub desired_version: u32,
    pub completed_version: Option<u32>,
    pub readiness: ProjectionReadiness,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPackSelectionValue {
    pub query_id: String,
    pub contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPackSelectionExpectation {
    Absent,
    At {
        selected: QueryPackSelectionValue,
        selection_epoch: u64,
    },
}

/// Same-transaction proof required before a query pack may select a projection
/// implementation. The projection readiness and complete source/fact-family
/// coverage must be the two halves of one durable barrier commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPackProjectionGuard {
    pub projection_id: String,
    pub projection_scope_key: Vec<u8>,
    pub projection_version: u32,
    pub coverage_owner_id: String,
    pub coverage_domain_name: String,
    pub coverage_domain_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPackSelectionUpdate {
    pub query_pack_id: String,
    pub scope_key: Vec<u8>,
    pub expected: QueryPackSelectionExpectation,
    pub selected: QueryPackSelectionValue,
    pub rollback: QueryPackSelectionValue,
    pub projection_guard: Option<QueryPackProjectionGuard>,
}

/// Writer-owned administrative transition for one or more common projection
/// packs. It advances the same durable commit clock as source ingestion while
/// touching no source cursor or adapter-owned object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionVersionCommit {
    pub source_instance_id: u64,
    pub reason: String,
    pub started_at: i64,
    pub committed_at: i64,
    pub projection_versions: Vec<ProjectionVersionUpdate>,
    pub coverage_sets: Vec<DurableCoverageSetUpdate>,
    pub coverage_preconditions: Vec<source_coverage::DurableCoverageSetPrecondition>,
    pub query_pack_selections: Vec<QueryPackSelectionUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    pub topic: String,
    pub schema_version: ChangeSchemaVersion,
    pub entity_key: Vec<u8>,
    pub operation: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecordError {
    pub generation: u64,
    pub cursor_start: Vec<u8>,
    pub cursor_end: Vec<u8>,
    pub payload_hash: Vec<u8>,
    pub media_type: String,
    pub raw_payload: Option<Vec<u8>>,
    pub error_class: String,
    pub error_message: String,
    pub adapter_version: String,
    pub contract_version: u32,
    pub last_retry_at: Option<i64>,
}

/// One source-range commit. The current Phase 2 caller supplies the number of
/// decoded facts; later typed projectors consume their fact batch inside this
/// same transaction before the cursor and outbox are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCommit {
    pub source: SourceInstanceSpec,
    pub stream: SourceStreamSpec,
    pub object: SourceObjectUpdate,
    pub reason: String,
    pub started_at: i64,
    pub committed_at: i64,
    pub fact_count: u32,
    pub projection_versions: Vec<ProjectionVersionUpdate>,
    pub record_errors: Vec<SourceRecordError>,
    pub changes: Vec<ChangeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitReceipt {
    pub commit_seq: u64,
    pub source_instance_id: u64,
    pub source_stream_id: u64,
    pub source_object_id: u64,
    pub change_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionVersionReceipt {
    pub commit_seq: u64,
    pub source_instance_id: u64,
}

/// Deterministic transaction seams used by process-like crash tests. The
/// post-commit points deliberately return an error after durability, modeling
/// a process that dies before acknowledging or publishing the commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommitStage {
    BeforeTransaction,
    MidCanonicalProjection,
    MidRuntimeProjection,
    MidUsageProjection,
    AfterCursorUpdate,
    AfterOutboxInsert,
    BeforeCommit,
    AfterCommit,
    BeforePublish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommitDetail {
    HistoryAndFactStorage,
    HistoryPreparation,
    FactStorage,
    CanonicalMessageStorage,
    HistoryProjectionWalk,
    ContentBlockStorage,
    DelegationProbe,
    DelegationProjection,
    DelegationReductions,
    ArtifactPreparation,
    ArtifactAssertionWrites,
    ArtifactReductions,
    ArtifactCleanup,
    SessionIndex,
    ProjectMemory,
    PersistedToolResult,
    InterpretationSettings,
    RunState,
    Delegation,
    Presence,
    Team,
    Task,
    Artifact,
    Workflow,
    UsageAggregation,
}

/// Deterministic seams around an administrative projection/coverage commit.
/// The final seam runs after SQLite durability but before the caller receives
/// its receipt, modeling process death during acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectionCommitStage {
    BeforeTransaction,
    AfterCommitInsert,
    AfterProjectionVersions,
    AfterCoverageReplacement,
    AfterQueryPackSelections,
    BeforeCommit,
    AfterCommit,
}

pub(super) trait CommitHook: Send + Sync {
    fn reach(&self, stage: CommitStage) -> Result<(), EngineError>;

    fn record_detail(&self, _detail: CommitDetail, _elapsed: Duration) {}
}

struct NoopCommitHook;

impl CommitHook for NoopCommitHook {
    fn reach(&self, _stage: CommitStage) -> Result<(), EngineError> {
        Ok(())
    }
}

pub(super) trait ProjectionCommitHook: Send + Sync {
    fn reach(&self, stage: ProjectionCommitStage) -> Result<(), EngineError>;
}

struct NoopProjectionCommitHook;

impl ProjectionCommitHook for NoopProjectionCommitHook {
    fn reach(&self, _stage: ProjectionCommitStage) -> Result<(), EngineError> {
        Ok(())
    }
}

/// Common-engine projection work that must share the source transaction.
/// This remains crate-private: adapters emit facts and never receive a SQL
/// callback. Later phases implement this trait on the common typed fact batch.
pub(super) trait TransactionalProjectionWork {
    fn apply_canonical(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError>;

    fn apply_runtime(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError>;

    fn apply_usage(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError>;
}

/// Stable provenance allocated before projection work, but visible to readers
/// only if the entire transaction commits.
pub(super) struct ProjectionCommitContext {
    pub commit_seq: u64,
    pub source_instance_id: u64,
    pub source_stream_id: u64,
    pub source_object_id: u64,
    pub generation: u64,
    /// True only for the first atomic commit that supersedes an object's
    /// previously durable generation. Same-generation append batches must not
    /// repeatedly search every projection for rows that cannot exist.
    pub replaces_prior_generation: bool,
    /// True when this transaction creates the source object. Replace-document
    /// projectors can skip ownership probes: a brand-new object cannot already
    /// own assertions.
    pub object_is_new: bool,
    /// The durable database is unavailable to readers until bootstrap
    /// finalization. Projectors may defer purely derived state that can be
    /// rebuilt atomically from their durable assertion tables at that gate.
    pub query_bootstrap: bool,
    pub consistency: ConsistencyPolicy,
    pub retention: RawRetentionPolicy,
}

impl ProjectionCommitContext {
    pub(super) fn skip_unowned_replace_document(&self, has_matching_fact: bool) -> bool {
        !has_matching_fact
            && (self.object_is_new
                || (!self.replaces_prior_generation
                    && self.consistency != ConsistencyPolicy::SnapshotReplace))
    }
}

struct NoProjectionWork;

impl TransactionalProjectionWork for NoProjectionWork {
    fn apply_canonical(
        &self,
        _transaction: &Transaction<'_>,
        _context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        Ok(Vec::new())
    }

    fn apply_runtime(
        &self,
        _transaction: &Transaction<'_>,
        _context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        Ok(Vec::new())
    }

    fn apply_usage(
        &self,
        _transaction: &Transaction<'_>,
        _context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        Ok(Vec::new())
    }
}

pub(crate) fn apply_observation_commit(
    connection: &mut Connection,
    request: &ObservationCommit,
) -> Result<CommitReceipt, EngineError> {
    apply_observation_commit_with_hook(connection, request, &NoopCommitHook)
}

pub(super) fn apply_observation_commit_with_hook(
    connection: &mut Connection,
    request: &ObservationCommit,
    hook: &dyn CommitHook,
) -> Result<CommitReceipt, EngineError> {
    apply_observation_commit_with_components(connection, request, &NoProjectionWork, hook)
}

/// Reserve or hydrate the durable identifier adapters need before emitting
/// entity keys. Discovery is catalog state, not a source-range commit, so it
/// deliberately does not allocate an ingest commit or change-log entry.
pub(super) fn reserve_source_instance(
    connection: &mut Connection,
    source: &SourceInstanceSpec,
) -> Result<u64, EngineError> {
    validate_source_instance(source)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin source instance reservation", error))?;
    let source_instance_id = upsert_source_instance(&transaction, source)?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit source instance reservation", error))?;
    Ok(source_instance_id)
}

/// Atomically advance common projection-pack readiness on the durable commit
/// clock without fabricating a source-object update. Equal transitions are a
/// true no-op so an unchanged reconciliation does not churn commit watermarks.
pub(super) fn apply_projection_version_commit(
    connection: &mut Connection,
    request: &ProjectionVersionCommit,
) -> Result<Option<ProjectionVersionReceipt>, EngineError> {
    apply_projection_version_commit_with_hook(connection, request, &NoopProjectionCommitHook)
}

pub(super) fn apply_projection_version_commit_with_hook(
    connection: &mut Connection,
    request: &ProjectionVersionCommit,
    hook: &dyn ProjectionCommitHook,
) -> Result<Option<ProjectionVersionReceipt>, EngineError> {
    validate_projection_version_commit(request)?;
    hook.reach(ProjectionCommitStage::BeforeTransaction)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin projection version commit", error))?;
    let source_instance_id = to_i64(request.source_instance_id, "source instance id")?;
    let source_exists = transaction
        .query_row(
            "SELECT 1 FROM source_instances WHERE source_instance_id = ?1",
            [source_instance_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| sqlite_error("validate projection source instance", error))?
        .is_some();
    if !source_exists {
        return Err(EngineError::InvalidCommit(
            "projection version commit references an unknown source instance".to_string(),
        ));
    }
    source_coverage::assert_preconditions(
        &transaction,
        request.source_instance_id,
        &request.coverage_preconditions,
    )?;
    let projection_versions_changed =
        projection_versions_changed(&transaction, &request.projection_versions)?;
    let coverage_changed = source_coverage::updates_changed(&transaction, &request.coverage_sets)?;
    let query_pack_selections_changed = query_pack_selections_changed(
        &transaction,
        request.source_instance_id,
        &request.query_pack_selections,
    )?;
    if !projection_versions_changed && !coverage_changed && !query_pack_selections_changed {
        transaction
            .commit()
            .map_err(|error| sqlite_error("finish unchanged projection version commit", error))?;
        return Ok(None);
    }

    transaction
        .execute(
            r#"
            INSERT INTO ingest_commits (
                source_instance_id, reason, started_at, committed_at, fact_count
            ) VALUES (?1, ?2, ?3, ?4, 0)
            "#,
            params![
                source_instance_id,
                request.reason,
                request.started_at,
                request.committed_at,
            ],
        )
        .map_err(|error| sqlite_error("insert projection version commit", error))?;
    hook.reach(ProjectionCommitStage::AfterCommitInsert)?;
    let commit_seq = from_i64(
        transaction.last_insert_rowid(),
        "projection version commit sequence",
    )?;
    write_projection_versions(
        &transaction,
        commit_seq,
        request.committed_at,
        &request.projection_versions,
    )?;
    hook.reach(ProjectionCommitStage::AfterProjectionVersions)?;
    source_coverage::replace_sets(
        &transaction,
        request.source_instance_id,
        commit_seq,
        request.committed_at,
        &request.coverage_sets,
    )?;
    hook.reach(ProjectionCommitStage::AfterCoverageReplacement)?;
    write_query_pack_selections(
        &transaction,
        request.source_instance_id,
        commit_seq,
        request.committed_at,
        &request.query_pack_selections,
    )?;
    hook.reach(ProjectionCommitStage::AfterQueryPackSelections)?;
    hook.reach(ProjectionCommitStage::BeforeCommit)?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit projection version transition", error))?;
    hook.reach(ProjectionCommitStage::AfterCommit)?;
    Ok(Some(ProjectionVersionReceipt {
        commit_seq,
        source_instance_id: request.source_instance_id,
    }))
}

pub(super) fn apply_observation_commit_with_projection(
    connection: &mut Connection,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
) -> Result<CommitReceipt, EngineError> {
    apply_observation_commit_with_projection_and_hook(
        connection,
        request,
        projection_work,
        &NoopCommitHook,
    )
}

pub(super) fn apply_observation_commit_with_projection_and_hook(
    connection: &mut Connection,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
    hook: &dyn CommitHook,
) -> Result<CommitReceipt, EngineError> {
    apply_observation_commit_with_components(connection, request, projection_work, hook)
}

pub(super) fn apply_observation_commit_with_components(
    connection: &mut Connection,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
    hook: &dyn CommitHook,
) -> Result<CommitReceipt, EngineError> {
    prepare_observation_commit(request, hook)?;

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin ingest commit", error))?;
    let receipt = apply_observation_commit_components_in_transaction(
        &transaction,
        request,
        projection_work,
        hook,
        true,
        false,
    )?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit ingest transaction", error))?;
    complete_observation_commit(hook)?;
    Ok(receipt)
}

pub(super) fn apply_observation_commit_in_transaction(
    transaction: &Transaction<'_>,
    request: &ObservationCommit,
    hook: &dyn CommitHook,
    persist_public_changes: bool,
    query_bootstrap: bool,
) -> Result<CommitReceipt, EngineError> {
    prepare_observation_commit(request, hook)?;
    apply_observation_commit_components_in_transaction(
        transaction,
        request,
        &NoProjectionWork,
        hook,
        persist_public_changes,
        query_bootstrap,
    )
}

pub(super) fn apply_observation_commit_with_projection_in_transaction(
    transaction: &Transaction<'_>,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
    hook: &dyn CommitHook,
    persist_public_changes: bool,
    query_bootstrap: bool,
) -> Result<CommitReceipt, EngineError> {
    prepare_observation_commit(request, hook)?;
    apply_observation_commit_components_in_transaction(
        transaction,
        request,
        projection_work,
        hook,
        persist_public_changes,
        query_bootstrap,
    )
}

fn prepare_observation_commit(
    request: &ObservationCommit,
    hook: &dyn CommitHook,
) -> Result<(), EngineError> {
    validate_commit(request)?;
    hook.reach(CommitStage::BeforeTransaction)
}

pub(super) fn complete_observation_commit(hook: &dyn CommitHook) -> Result<(), EngineError> {
    hook.reach(CommitStage::AfterCommit)?;
    hook.reach(CommitStage::BeforePublish)
}

fn apply_observation_commit_components_in_transaction(
    transaction: &Transaction<'_>,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
    hook: &dyn CommitHook,
    persist_public_changes: bool,
    query_bootstrap: bool,
) -> Result<CommitReceipt, EngineError> {
    let source_instance_id = upsert_source_instance(transaction, &request.source)?;
    let commit_seq = insert_ingest_commit(transaction, source_instance_id, request)?;
    let source_stream_id =
        upsert_source_stream(transaction, source_instance_id, commit_seq, &request.stream)?;
    let existing = read_source_object(transaction, source_stream_id, &request.object.object_key)?;
    verify_cursor_precondition(request, existing.as_ref())?;
    let object_is_new = existing.is_none();
    let source_object_id = reserve_source_object_identity(
        transaction,
        source_stream_id,
        &request.object,
        existing.as_ref(),
    )?;
    let projection_context = ProjectionCommitContext {
        commit_seq,
        source_instance_id,
        source_stream_id,
        source_object_id,
        generation: request.object.generation,
        replaces_prior_generation: existing
            .as_ref()
            .is_some_and(|stored| stored.generation != request.object.generation),
        object_is_new,
        query_bootstrap,
        consistency: request.stream.consistency,
        retention: request.stream.retention,
    };

    let mut changes = request.changes.clone();
    changes.extend(projection_work.apply_canonical(transaction, &projection_context)?);
    hook.reach(CommitStage::MidCanonicalProjection)?;
    changes.extend(projection_work.apply_runtime(transaction, &projection_context)?);
    hook.reach(CommitStage::MidRuntimeProjection)?;
    changes.extend(projection_work.apply_usage(transaction, &projection_context)?);
    hook.reach(CommitStage::MidUsageProjection)?;
    let changes = coalesce_changes(changes);
    validate_changes(&changes)?;

    finalize_source_object(transaction, source_object_id, commit_seq, &request.object)?;
    hook.reach(CommitStage::AfterCursorUpdate)?;

    write_projection_versions(
        transaction,
        commit_seq,
        request.committed_at,
        &request.projection_versions,
    )?;
    write_record_errors(
        transaction,
        source_object_id,
        commit_seq,
        &request.record_errors,
    )?;
    if persist_public_changes {
        write_change_log(transaction, commit_seq, &changes)?;
    }
    hook.reach(CommitStage::AfterOutboxInsert)?;

    if persist_public_changes
        && commit_seq % AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS == 0
        && automatic_change_log_maintenance_due(
            transaction,
            ChangeLogRetentionPolicy::default(),
            request.committed_at,
            commit_seq,
        )?
    {
        prune_change_log(
            transaction,
            ChangeLogRetentionPolicy::default(),
            request.committed_at,
        )?;
    }
    hook.reach(CommitStage::BeforeCommit)?;

    Ok(CommitReceipt {
        commit_seq,
        source_instance_id,
        source_stream_id,
        source_object_id,
        change_count: u32::try_from(changes.len()).unwrap_or(u32::MAX),
    })
}

fn automatic_change_log_maintenance_due(
    transaction: &Transaction<'_>,
    policy: ChangeLogRetentionPolicy,
    now_ms: i64,
    watermark: u64,
) -> Result<bool, EngineError> {
    let (current_floor, retained_payload_bytes, last_pruned_at): (i64, i64, Option<i64>) =
        transaction
            .query_row(
                r#"
                SELECT pruned_through_commit_seq, retained_payload_bytes, last_pruned_at
                FROM change_log_retention_state WHERE singleton = 1
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| sqlite_error("read automatic retention schedule", error))?;
    let current_floor = from_i64(current_floor, "automatic retention floor")?;
    let retained_payload_bytes =
        from_i64(retained_payload_bytes, "automatic retained payload bytes")?;
    let protected_floor = watermark.saturating_sub(policy.min_resumable_commits);
    let size_due =
        retained_payload_bytes > policy.max_payload_bytes && protected_floor > current_floor;
    let age_due = last_pruned_at.is_none_or(|last| {
        now_ms.saturating_sub(last) >= AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_MS
    });
    Ok(size_due || age_due)
}

fn coalesce_changes(changes: Vec<ChangeEntry>) -> Vec<ChangeEntry> {
    let mut positions = std::collections::BTreeMap::new();
    let mut coalesced = Vec::with_capacity(changes.len());
    for change in changes {
        let key = (change.topic.clone(), change.entity_key.clone());
        if let Some(position) = positions.get(&key).copied() {
            coalesced[position] = change;
        } else {
            positions.insert(key, coalesced.len());
            coalesced.push(change);
        }
    }
    coalesced
}

#[derive(Debug, Clone)]
struct StoredSourceObject {
    source_object_id: u64,
    generation: u64,
    committed_cursor: Vec<u8>,
}

fn validate_commit(request: &ObservationCommit) -> Result<(), EngineError> {
    validate_source_instance(&request.source)?;
    require_text("stream.stream_key", &request.stream.stream_key)?;
    require_text("stream.driver_kind", &request.stream.driver_kind)?;
    require_text("stream.decoder_key", &request.stream.decoder_key)?;
    require_text("stream.stream_state", &request.stream.stream_state)?;
    require_bytes("object.object_key", &request.object.object_key)?;
    require_text("object.state", &request.object.state)?;
    require_text("reason", &request.reason)?;
    to_i64(request.object.generation, "source object generation")?;
    if let Some(size) = request.object.size_bytes {
        to_i64(size, "source object size")?;
    }
    match (
        request.object.driver_checkpoint.as_deref(),
        request.object.driver_checkpoint_version,
    ) {
        (Some(checkpoint), Some(version)) => {
            require_bytes("object.driver_checkpoint", checkpoint)?;
            if checkpoint.len() > MAX_DRIVER_CHECKPOINT_BYTES {
                return Err(EngineError::InvalidCommit(format!(
                    "driver checkpoint exceeds {MAX_DRIVER_CHECKPOINT_BYTES} bytes"
                )));
            }
            if version == 0 {
                return Err(EngineError::InvalidCommit(
                    "driver checkpoint version must be greater than zero".to_string(),
                ));
            }
        }
        (None, None) => {}
        _ => {
            return Err(EngineError::InvalidCommit(
                "driver checkpoint and version must be present together".to_string(),
            ));
        }
    }
    if request.committed_at < request.started_at {
        return Err(EngineError::InvalidCommit(
            "committed_at must not precede started_at".to_string(),
        ));
    }

    if let ExpectedSourceCursor::At { generation, .. } = &request.object.expected {
        to_i64(*generation, "expected source object generation")?;
        if request.object.generation < *generation {
            return Err(EngineError::InvalidCommit(
                "source object generation must not move backwards".to_string(),
            ));
        }
    }

    validate_projection_versions(&request.projection_versions)?;

    validate_changes(&request.changes)?;

    for error in &request.record_errors {
        require_bytes("record_error.payload_hash", &error.payload_hash)?;
        require_text("record_error.media_type", &error.media_type)?;
        require_text("record_error.error_class", &error.error_class)?;
        require_text("record_error.error_message", &error.error_message)?;
        require_text("record_error.adapter_version", &error.adapter_version)?;
        to_i64(error.generation, "record error generation")?;
    }
    Ok(())
}

fn validate_projection_version_commit(
    request: &ProjectionVersionCommit,
) -> Result<(), EngineError> {
    to_i64(request.source_instance_id, "source instance id")?;
    require_text("projection commit reason", &request.reason)?;
    if request.committed_at < request.started_at {
        return Err(EngineError::InvalidCommit(
            "projection commit time must not precede its start".to_string(),
        ));
    }
    if request.projection_versions.is_empty() && request.query_pack_selections.is_empty() {
        return Err(EngineError::InvalidCommit(
            "projection version commit requires a projection or query-pack update".to_string(),
        ));
    }
    if !request.query_pack_selections.is_empty()
        && (!request.projection_versions.is_empty()
            || !request.coverage_sets.is_empty()
            || !request.coverage_preconditions.is_empty())
    {
        return Err(EngineError::InvalidCommit(
            "query-pack selection must be an isolated administrative transaction".to_string(),
        ));
    }
    validate_projection_versions(&request.projection_versions)?;
    validate_query_pack_selections(&request.query_pack_selections)?;
    source_coverage::validate_updates(&request.coverage_sets)?;
    source_coverage::validate_preconditions(&request.coverage_preconditions)?;
    for coverage in &request.coverage_sets {
        if !request.projection_versions.iter().any(|projection| {
            projection.projection_id == coverage.owner_id
                && projection.scope_key == coverage.owner_scope_key
        }) {
            return Err(EngineError::InvalidCommit(
                "source coverage update has no matching projection owner transition".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_projection_versions(
    projections: &[ProjectionVersionUpdate],
) -> Result<(), EngineError> {
    if projections.len() > MAX_PROJECTION_VERSION_UPDATES {
        return Err(EngineError::InvalidCommit(format!(
            "projection update count exceeds {MAX_PROJECTION_VERSION_UPDATES}"
        )));
    }
    let mut identities = BTreeSet::new();
    for projection in projections {
        require_text("projection.projection_id", &projection.projection_id)?;
        require_bytes("projection.scope_key", &projection.scope_key)?;
        if projection.scope_key.len() > MAX_PROJECTION_SCOPE_KEY_BYTES {
            return Err(EngineError::InvalidCommit(format!(
                "projection scope key exceeds {MAX_PROJECTION_SCOPE_KEY_BYTES} bytes"
            )));
        }
        if let Some(detail) = &projection.detail {
            require_text("projection.detail", detail)?;
        }
        if !identities.insert((&projection.projection_id, &projection.scope_key)) {
            return Err(EngineError::InvalidCommit(format!(
                "projection {} updates the same scope more than once",
                projection.projection_id
            )));
        }
        if projection.desired_version == 0 {
            return Err(EngineError::InvalidCommit(
                "projection desired_version must be greater than zero".to_string(),
            ));
        }
        match projection.readiness {
            ProjectionReadiness::Ready
                if projection.completed_version != Some(projection.desired_version) =>
            {
                return Err(EngineError::InvalidCommit(format!(
                    "ready projection {} must complete its desired version",
                    projection.projection_id
                )));
            }
            ProjectionReadiness::StaleSafe if projection.completed_version.is_none() => {
                return Err(EngineError::InvalidCommit(format!(
                    "stale-safe projection {} requires a completed version",
                    projection.projection_id
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_query_pack_selections(
    selections: &[QueryPackSelectionUpdate],
) -> Result<(), EngineError> {
    if selections.len() > MAX_QUERY_PACK_SELECTION_UPDATES {
        return Err(EngineError::InvalidCommit(format!(
            "query-pack selection count exceeds {MAX_QUERY_PACK_SELECTION_UPDATES}"
        )));
    }
    let mut identities = BTreeSet::new();
    for selection in selections {
        validate_query_pack_id("query_pack.query_pack_id", &selection.query_pack_id)?;
        require_bytes("query_pack.scope_key", &selection.scope_key)?;
        if selection.scope_key.len() > MAX_PROJECTION_SCOPE_KEY_BYTES {
            return Err(EngineError::InvalidCommit(format!(
                "query-pack scope key exceeds {MAX_PROJECTION_SCOPE_KEY_BYTES} bytes"
            )));
        }
        if !identities.insert((&selection.query_pack_id, &selection.scope_key)) {
            return Err(EngineError::InvalidCommit(format!(
                "query pack {} updates the same scope more than once",
                selection.query_pack_id
            )));
        }
        validate_query_pack_value("query_pack.selected", &selection.selected)?;
        validate_query_pack_value("query_pack.rollback", &selection.rollback)?;
        if let QueryPackSelectionExpectation::At {
            selected,
            selection_epoch,
        } = &selection.expected
        {
            validate_query_pack_value("query_pack.expected", selected)?;
            if *selection_epoch == 0 {
                return Err(EngineError::InvalidCommit(
                    "query-pack expected selection epoch must be greater than zero".to_string(),
                ));
            }
            to_i64(*selection_epoch, "query-pack expected selection epoch")?;
        }
        if let Some(guard) = &selection.projection_guard {
            validate_query_pack_id("query_pack.guard.projection_id", &guard.projection_id)?;
            require_bytes(
                "query_pack.guard.projection_scope_key",
                &guard.projection_scope_key,
            )?;
            if guard.projection_scope_key.len() > MAX_PROJECTION_SCOPE_KEY_BYTES {
                return Err(EngineError::InvalidCommit(format!(
                    "query-pack projection scope key exceeds {MAX_PROJECTION_SCOPE_KEY_BYTES} bytes"
                )));
            }
            if guard.projection_version == 0 || guard.coverage_domain_version == 0 {
                return Err(EngineError::InvalidCommit(
                    "query-pack projection and coverage versions must be greater than zero"
                        .to_string(),
                ));
            }
            if guard.projection_scope_key != selection.scope_key {
                return Err(EngineError::InvalidCommit(
                    "query-pack guard and selection must use the same source scope".to_string(),
                ));
            }
            validate_query_pack_id(
                "query_pack.guard.coverage_owner_id",
                &guard.coverage_owner_id,
            )?;
            validate_query_pack_id(
                "query_pack.guard.coverage_domain_name",
                &guard.coverage_domain_name,
            )?;
        }
    }
    Ok(())
}

fn validate_query_pack_value(
    field: &'static str,
    value: &QueryPackSelectionValue,
) -> Result<(), EngineError> {
    validate_query_pack_id(field, &value.query_id)?;
    if value.contract_version == 0 {
        return Err(EngineError::InvalidCommit(format!(
            "{field} contract version must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_query_pack_id(field: &'static str, value: &str) -> Result<(), EngineError> {
    require_text(field, value)?;
    if value.len() > MAX_QUERY_PACK_ID_BYTES {
        return Err(EngineError::InvalidCommit(format!(
            "{field} exceeds {MAX_QUERY_PACK_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_source_instance(source: &SourceInstanceSpec) -> Result<(), EngineError> {
    require_text("source.adapter_id", &source.adapter_id)?;
    require_bytes("source.stable_key", &source.stable_key)?;
    require_text("source.display_name", &source.display_name)?;
    require_text("source.adapter_version", &source.adapter_version)?;
    if source.adapter_contract_version == 0 {
        return Err(EngineError::InvalidCommit(
            "source adapter contract version must be greater than zero".to_string(),
        ));
    }
    if source.last_seen_at < source.discovered_at {
        return Err(EngineError::InvalidCommit(
            "source last_seen_at must not precede discovered_at".to_string(),
        ));
    }
    let mut source_schema_versions = BTreeSet::new();
    for version in &source.source_schema_versions {
        require_text("source.source_schema_version", version)?;
        if !source_schema_versions.insert(version) {
            return Err(EngineError::InvalidCommit(format!(
                "source schema version {version} is declared more than once"
            )));
        }
    }
    let mut capability_ids = BTreeSet::new();
    for capability in &source.capabilities {
        require_text("source.capability.id", &capability.id)?;
        require_text("source.capability.support_level", &capability.support_level)?;
        require_text("source.capability.granularity", &capability.granularity)?;
        require_text("source.capability.availability", &capability.availability)?;
        if !capability_ids.insert(&capability.id) {
            return Err(EngineError::InvalidCommit(format!(
                "source capability {} is declared more than once",
                capability.id
            )));
        }
    }
    Ok(())
}

fn validate_changes(changes: &[ChangeEntry]) -> Result<(), EngineError> {
    for change in changes {
        require_text("change.topic", &change.topic)?;
        require_text("change.operation", &change.operation)?;
        require_bytes("change.entity_key", &change.entity_key)?;
        if change.schema_version == 0 {
            return Err(EngineError::InvalidCommit(
                "change schema_version must be greater than zero".to_string(),
            ));
        }
    }
    Ok(())
}

fn require_text(field: &'static str, value: &str) -> Result<(), EngineError> {
    if value.trim().is_empty() {
        Err(EngineError::InvalidCommit(format!(
            "{field} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn require_bytes(field: &'static str, value: &[u8]) -> Result<(), EngineError> {
    if value.is_empty() {
        Err(EngineError::InvalidCommit(format!(
            "{field} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn upsert_source_instance(
    transaction: &Transaction<'_>,
    source: &SourceInstanceSpec,
) -> Result<u64, EngineError> {
    let source_schema_versions_json = serde_json::to_string(&source.source_schema_versions)
        .map_err(|error| {
            EngineError::InvalidCommit(format!("could not encode source schema versions: {error}"))
        })?;
    let capabilities_json = serde_json::to_string(&source.capabilities).map_err(|error| {
        EngineError::InvalidCommit(format!("could not encode source capabilities: {error}"))
    })?;
    let existing_id: Option<i64> = transaction
        .query_row(
            "SELECT source_instance_id FROM source_instances WHERE adapter_id = ?1 AND stable_key = ?2",
            params![source.adapter_id, source.stable_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite_error("read source instance identity", error))?;
    let source_instance_id = match existing_id {
        Some(id) => {
            let source_instance_id = from_i64(id, "source instance id")?;
            transaction
                .execute(
                    r#"
                    UPDATE source_instances
                    SET display_name = ?2,
                        adapter_version = ?3,
                        adapter_contract_version = ?4,
                        source_schema_versions_json = ?5,
                        capabilities_json = ?6,
                        last_seen_at = ?7
                    WHERE source_instance_id = ?1
                      AND (
                        display_name IS NOT ?2 OR
                        adapter_version IS NOT ?3 OR
                        adapter_contract_version IS NOT ?4 OR
                        source_schema_versions_json IS NOT ?5 OR
                        capabilities_json IS NOT ?6 OR
                        last_seen_at IS NOT ?7
                      )
                    "#,
                    params![
                        id,
                        source.display_name,
                        source.adapter_version,
                        i64::from(source.adapter_contract_version),
                        source_schema_versions_json,
                        capabilities_json,
                        source.last_seen_at,
                    ],
                )
                .map_err(|error| sqlite_error("refresh changed source instance", error))?;
            return Ok(source_instance_id);
        }
        None => {
            let id = source_instance_catalog_id(&source.adapter_id, &source.stable_key);
            let occupied: Option<(String, Vec<u8>)> = transaction
                .query_row(
                    "SELECT adapter_id, stable_key FROM source_instances WHERE source_instance_id = ?1",
                    [to_i64(id, "source instance id")?],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|error| sqlite_error("check source instance identity collision", error))?;
            if occupied.is_some() {
                return Err(EngineError::InvalidCommit(
                    "deterministic source instance identity collision".to_string(),
                ));
            }
            id
        }
    };
    transaction
        .execute(
            r#"
            INSERT INTO source_instances (
                source_instance_id, adapter_id, stable_key, display_name, adapter_version,
                adapter_contract_version, source_schema_versions_json,
                capabilities_json, discovered_at, last_seen_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                to_i64(source_instance_id, "source instance id")?,
                source.adapter_id,
                source.stable_key,
                source.display_name,
                source.adapter_version,
                i64::from(source.adapter_contract_version),
                source_schema_versions_json,
                capabilities_json,
                source.discovered_at,
                source.last_seen_at
            ],
        )
        .map_err(|error| sqlite_error("insert source instance", error))?;
    Ok(source_instance_id)
}

fn insert_ingest_commit(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    request: &ObservationCommit,
) -> Result<u64, EngineError> {
    let id: i64 = transaction
        .query_row(
            r#"
            INSERT INTO ingest_commits (
                source_instance_id, reason, started_at, committed_at, fact_count
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            RETURNING commit_seq
            "#,
            params![
                to_i64(source_instance_id, "source instance id")?,
                request.reason,
                request.started_at,
                request.committed_at,
                i64::from(request.fact_count),
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("allocate ingest commit", error))?;
    from_i64(id, "commit sequence")
}

fn upsert_source_stream(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    commit_seq: u64,
    stream: &SourceStreamSpec,
) -> Result<u64, EngineError> {
    let existing_id: Option<i64> = transaction
        .query_row(
            "SELECT source_stream_id FROM source_streams WHERE source_instance_id = ?1 AND stream_key = ?2",
            params![to_i64(source_instance_id, "source instance id")?, stream.stream_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite_error("read source stream identity", error))?;
    let source_stream_id = match existing_id {
        Some(id) => from_i64(id, "source stream id")?,
        None => {
            let id = source_stream_catalog_id(source_instance_id, &stream.stream_key);
            let occupied: Option<i64> = transaction
                .query_row(
                    "SELECT source_stream_id FROM source_streams WHERE source_stream_id = ?1",
                    [to_i64(id, "source stream id")?],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| sqlite_error("check source stream identity collision", error))?;
            if occupied.is_some() {
                return Err(EngineError::InvalidCommit(
                    "deterministic source stream identity collision".to_string(),
                ));
            }
            id
        }
    };
    let id: i64 = transaction
        .query_row(
            r#"
            INSERT INTO source_streams (
                source_stream_id, source_instance_id, stream_key, driver_kind, decoder_key,
                stream_state, raw_retention, last_reconciled_at, last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(source_instance_id, stream_key) DO UPDATE SET
                driver_kind = excluded.driver_kind,
                decoder_key = excluded.decoder_key,
                stream_state = excluded.stream_state,
                raw_retention = excluded.raw_retention,
                last_reconciled_at = excluded.last_reconciled_at,
                last_commit_seq = excluded.last_commit_seq
            RETURNING source_stream_id
            "#,
            params![
                to_i64(source_stream_id, "source stream id")?,
                to_i64(source_instance_id, "source instance id")?,
                stream.stream_key,
                stream.driver_kind,
                stream.decoder_key,
                stream.stream_state,
                raw_retention_policy(stream.retention),
                stream.last_reconciled_at,
                to_i64(commit_seq, "commit sequence")?
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("upsert source stream", error))?;
    from_i64(id, "source stream id")
}

fn raw_retention_policy(policy: RawRetentionPolicy) -> &'static str {
    match policy {
        RawRetentionPolicy::None => "none",
        RawRetentionPolicy::HashOnly => "hash_only",
        RawRetentionPolicy::DiagnosticExcerpt => "diagnostic_excerpt",
        RawRetentionPolicy::Full => "full",
    }
}

fn read_source_object(
    transaction: &Transaction<'_>,
    source_stream_id: u64,
    object_key: &[u8],
) -> Result<Option<StoredSourceObject>, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT source_object_id, generation, committed_cursor
            FROM source_objects
            WHERE source_stream_id = ?1 AND object_key = ?2
            "#,
            params![to_i64(source_stream_id, "source stream id")?, object_key],
            |row| {
                let id: i64 = row.get(0)?;
                let generation: i64 = row.get(1)?;
                Ok((id, generation, row.get(2)?))
            },
        )
        .optional()
        .map_err(|error| sqlite_error("read source object cursor", error))?
        .map(|(id, generation, committed_cursor)| {
            Ok(StoredSourceObject {
                source_object_id: from_i64(id, "source object id")?,
                generation: from_i64(generation, "source object generation")?,
                committed_cursor,
            })
        })
        .transpose()
}

fn verify_cursor_precondition(
    request: &ObservationCommit,
    existing: Option<&StoredSourceObject>,
) -> Result<(), EngineError> {
    let matches = match (&request.object.expected, existing) {
        (ExpectedSourceCursor::Absent, None) => true,
        (
            ExpectedSourceCursor::At {
                generation,
                committed_cursor,
            },
            Some(stored),
        ) => stored.generation == *generation && stored.committed_cursor == *committed_cursor,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(EngineError::StaleSourceCursor {
            adapter_id: request.source.adapter_id.clone(),
            stream_key: request.stream.stream_key.clone(),
        })
    }
}

fn reserve_source_object_identity(
    transaction: &Transaction<'_>,
    source_stream_id: u64,
    object: &SourceObjectUpdate,
    existing: Option<&StoredSourceObject>,
) -> Result<u64, EngineError> {
    if let Some(existing) = existing {
        return Ok(existing.source_object_id);
    }

    let source_object_id = source_object_catalog_id(source_stream_id, &object.object_key);
    let occupied: Option<i64> = transaction
        .query_row(
            "SELECT source_object_id FROM source_objects WHERE source_object_id = ?1",
            [to_i64(source_object_id, "source object id")?],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite_error("check source object identity collision", error))?;
    if occupied.is_some() {
        return Err(EngineError::InvalidCommit(
            "deterministic source object identity collision".to_string(),
        ));
    }
    let id: i64 = transaction
        .query_row(
            r#"
            INSERT INTO source_objects (
                source_object_id, source_stream_id, object_key, generation, committed_cursor,
                decoder_contract_version, state
            ) VALUES (?1, ?2, ?3, ?4, X'', ?5, 'pending')
            RETURNING source_object_id
            "#,
            params![
                to_i64(source_object_id, "source object id")?,
                to_i64(source_stream_id, "source stream id")?,
                object.object_key,
                to_i64(object.generation, "source object generation")?,
                i64::from(object.decoder_contract_version),
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("reserve source object identity", error))?;
    from_i64(id, "source object id")
}

fn deterministic_catalog_id(domain: &[u8], components: &[&[u8]]) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/catalog-id/v1");
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    for component in components {
        hasher.update(&(component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&hasher.finalize().as_bytes()[..8]);
    // Catalog IDs cross N-API as JavaScript numbers in the current transport,
    // so keep the deterministic namespace inside the exact integer range.
    let id = u64::from_be_bytes(prefix) & ((1_u64 << 53) - 1);
    id.max(1)
}

pub(crate) fn source_instance_catalog_id(adapter_id: &str, stable_key: &[u8]) -> u64 {
    deterministic_catalog_id(b"source-instance", &[adapter_id.as_bytes(), stable_key])
}

pub(crate) fn source_stream_catalog_id(source_instance_id: u64, stream_key: &str) -> u64 {
    deterministic_catalog_id(
        b"source-stream",
        &[&source_instance_id.to_be_bytes(), stream_key.as_bytes()],
    )
}

pub(crate) fn source_object_catalog_id(source_stream_id: u64, object_key: &[u8]) -> u64 {
    deterministic_catalog_id(
        b"source-object",
        &[&source_stream_id.to_be_bytes(), object_key],
    )
}

fn finalize_source_object(
    transaction: &Transaction<'_>,
    source_object_id: u64,
    commit_seq: u64,
    object: &SourceObjectUpdate,
) -> Result<(), EngineError> {
    let size_bytes = object
        .size_bytes
        .map(|value| to_i64(value, "source object size"))
        .transpose()?;
    let updated = transaction
        .execute(
            r#"
            UPDATE source_objects SET
                display_path = ?1, native_identity = ?2, generation = ?3,
                committed_cursor = ?4, observed_revision = ?5,
                adapter_object_context = ?6, driver_checkpoint = ?7,
                driver_checkpoint_version = ?8, decoder_state = ?9,
                decoder_state_version = ?10, retry_state = ?11,
                size_bytes = ?12, mtime_ns = ?13,
                decoder_contract_version = ?14, last_commit_seq = ?15,
                state = ?16
            WHERE source_object_id = ?17
            "#,
            params![
                object.display_path,
                object.native_identity,
                to_i64(object.generation, "source object generation")?,
                object.committed_cursor,
                object.observed_revision,
                object.adapter_object_context,
                object.driver_checkpoint,
                object.driver_checkpoint_version.map(i64::from),
                object.decoder_state,
                object.decoder_state_version.map(i64::from),
                object.retry_state,
                size_bytes,
                object.mtime_ns,
                i64::from(object.decoder_contract_version),
                to_i64(commit_seq, "commit sequence")?,
                object.state,
                to_i64(source_object_id, "source object id")?,
            ],
        )
        .map_err(|error| sqlite_error("finalize source object cursor", error))?;
    if updated != 1 {
        return Err(EngineError::Sqlite {
            operation: "finalize source object cursor",
            detail: format!("expected one source object row, updated {updated}"),
        });
    }
    Ok(())
}

fn write_projection_versions(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    updated_at: i64,
    projections: &[ProjectionVersionUpdate],
) -> Result<(), EngineError> {
    let mut statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO projection_versions (
                projection_id, scope_key, desired_version, completed_version,
                readiness, last_commit_seq, updated_at, detail
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(projection_id, scope_key) DO UPDATE SET
                desired_version = excluded.desired_version,
                completed_version = excluded.completed_version,
                readiness = excluded.readiness,
                last_commit_seq = excluded.last_commit_seq,
                updated_at = excluded.updated_at,
                detail = excluded.detail
            "#,
        )
        .map_err(|error| sqlite_error("prepare projection version update", error))?;
    for projection in projections {
        statement
            .execute(params![
                projection.projection_id,
                projection.scope_key,
                i64::from(projection.desired_version),
                projection.completed_version.map(i64::from),
                projection.readiness.as_str(),
                to_i64(commit_seq, "commit sequence")?,
                updated_at,
                projection.detail,
            ])
            .map_err(|error| sqlite_error("write projection version", error))?;
    }
    Ok(())
}

fn projection_versions_changed(
    transaction: &Transaction<'_>,
    projections: &[ProjectionVersionUpdate],
) -> Result<bool, EngineError> {
    let mut statement = transaction
        .prepare_cached(
            r#"
            SELECT desired_version, completed_version, readiness, detail
            FROM projection_versions
            WHERE projection_id = ?1 AND scope_key = ?2
            "#,
        )
        .map_err(|error| sqlite_error("prepare projection version comparison", error))?;
    for projection in projections {
        let current = statement
            .query_row(
                params![projection.projection_id, projection.scope_key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| sqlite_error("compare projection version", error))?;
        let desired = i64::from(projection.desired_version);
        let completed = projection.completed_version.map(i64::from);
        if current.as_ref()
            != Some(&(
                desired,
                completed,
                projection.readiness.as_str().to_string(),
                projection.detail.clone(),
            ))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredQueryPackSelection {
    source_instance_id: u64,
    selected: QueryPackSelectionValue,
    rollback: QueryPackSelectionValue,
    selection_epoch: u64,
}

fn query_pack_selections_changed(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    selections: &[QueryPackSelectionUpdate],
) -> Result<bool, EngineError> {
    let mut changed = false;
    for selection in selections {
        let current = read_query_pack_selection(
            transaction,
            source_instance_id,
            &selection.query_pack_id,
            &selection.scope_key,
        )?;
        let target_is_current = current.as_ref().is_some_and(|current| {
            current.source_instance_id == source_instance_id
                && current.selected == selection.selected
                && current.rollback == selection.rollback
        });
        // This ordering makes a retry after post-commit acknowledgement loss
        // idempotent even though its expected old epoch is now stale.
        if target_is_current {
            continue;
        }
        match (&selection.expected, &current) {
            (QueryPackSelectionExpectation::Absent, None) => {}
            (
                QueryPackSelectionExpectation::At {
                    selected,
                    selection_epoch,
                },
                Some(current),
            ) if current.source_instance_id == source_instance_id
                && current.selected == *selected
                && current.selection_epoch == *selection_epoch => {}
            _ => {
                return Err(EngineError::InvalidCommit(format!(
                    "query pack {} selection compare-and-set failed",
                    selection.query_pack_id
                )));
            }
        }
        let is_explicit_rollback = current.as_ref().is_some_and(|current| {
            selection.selected == current.rollback && selection.rollback == current.rollback
        });
        if !is_explicit_rollback && selection.projection_guard.is_none() {
            return Err(EngineError::InvalidCommit(format!(
                "query pack {} promotion requires a projection guard",
                selection.query_pack_id
            )));
        }
        if let Some(guard) = &selection.projection_guard {
            assert_query_pack_projection_guard(transaction, source_instance_id, guard)?;
        }
        changed = true;
    }
    Ok(changed)
}

fn read_query_pack_selection(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    query_pack_id: &str,
    scope_key: &[u8],
) -> Result<Option<StoredQueryPackSelection>, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT source_instance_id, selected_query_id,
                   selected_contract_version, rollback_query_id,
                   rollback_contract_version, selection_epoch
            FROM query_pack_selections
            WHERE source_instance_id = ?1
              AND query_pack_id = ?2
              AND scope_key = ?3
            "#,
            params![
                to_i64(source_instance_id, "source instance id")?,
                query_pack_id,
                scope_key,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_error("read query-pack selection", error))?
        .map(
            |(
                stored_source_instance_id,
                selected_query_id,
                selected_contract_version,
                rollback_query_id,
                rollback_contract_version,
                selection_epoch,
            )| {
                Ok(StoredQueryPackSelection {
                    source_instance_id: from_i64(
                        stored_source_instance_id,
                        "query-pack source instance id",
                    )?,
                    selected: QueryPackSelectionValue {
                        query_id: selected_query_id,
                        contract_version: query_pack_version(
                            selected_contract_version,
                            "selected query contract version",
                        )?,
                    },
                    rollback: QueryPackSelectionValue {
                        query_id: rollback_query_id,
                        contract_version: query_pack_version(
                            rollback_contract_version,
                            "rollback query contract version",
                        )?,
                    },
                    selection_epoch: from_i64(selection_epoch, "query-pack selection epoch")?,
                })
            },
        )
        .transpose()
}

fn query_pack_version(value: i64, field: &'static str) -> Result<u32, EngineError> {
    u32::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode query-pack selection",
        detail: format!("{field} is outside the supported unsigned range"),
    })
}

fn assert_query_pack_projection_guard(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    guard: &QueryPackProjectionGuard,
) -> Result<(), EngineError> {
    let satisfied = transaction
        .query_row(
            r#"
            SELECT 1
            FROM projection_versions AS projection
            JOIN source_coverage_sets AS coverage
              ON coverage.source_instance_id = ?1
             AND coverage.owner_id = ?2
             AND coverage.owner_scope_key = projection.scope_key
             AND coverage.domain_kind = 'fact_family'
             AND coverage.domain_name = ?3
             AND coverage.domain_version = ?4
             AND length(coverage.root_entity_key) = 0
            WHERE projection.projection_id = ?5
              AND projection.scope_key = ?6
              AND projection.desired_version = ?7
              AND projection.completed_version = ?7
              AND projection.readiness = 'ready'
              AND projection.last_commit_seq IS NOT NULL
              AND coverage.completeness = 'complete'
              AND coverage.last_commit_seq = projection.last_commit_seq
              AND NOT EXISTS (
                  SELECT 1
                  FROM source_coverage_errors AS errors
                  WHERE errors.coverage_set_id = coverage.coverage_set_id
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM source_coverage_points AS points
                  WHERE points.coverage_set_id = coverage.coverage_set_id
                    AND points.status NOT IN ('complete_through', 'exact_snapshot')
              )
            "#,
            params![
                to_i64(source_instance_id, "source instance id")?,
                guard.coverage_owner_id,
                guard.coverage_domain_name,
                i64::from(guard.coverage_domain_version),
                guard.projection_id,
                guard.projection_scope_key,
                i64::from(guard.projection_version),
            ],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| sqlite_error("verify query-pack projection guard", error))?
        .is_some();
    if !satisfied {
        return Err(EngineError::InvalidCommit(format!(
            "query-pack projection guard for {} version {} is not satisfied",
            guard.projection_id, guard.projection_version
        )));
    }
    Ok(())
}

fn write_query_pack_selections(
    transaction: &Transaction<'_>,
    source_instance_id: u64,
    commit_seq: u64,
    updated_at: i64,
    selections: &[QueryPackSelectionUpdate],
) -> Result<(), EngineError> {
    let mut statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO query_pack_selections (
                source_instance_id, query_pack_id, scope_key,
                selected_query_id, selected_contract_version,
                rollback_query_id, rollback_contract_version,
                selection_epoch, last_commit_seq, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(source_instance_id, query_pack_id, scope_key) DO UPDATE SET
                source_instance_id = excluded.source_instance_id,
                selected_query_id = excluded.selected_query_id,
                selected_contract_version = excluded.selected_contract_version,
                rollback_query_id = excluded.rollback_query_id,
                rollback_contract_version = excluded.rollback_contract_version,
                selection_epoch = excluded.selection_epoch,
                last_commit_seq = excluded.last_commit_seq,
                updated_at = excluded.updated_at
            "#,
        )
        .map_err(|error| sqlite_error("prepare query-pack selection update", error))?;
    for selection in selections {
        let current = read_query_pack_selection(
            transaction,
            source_instance_id,
            &selection.query_pack_id,
            &selection.scope_key,
        )?;
        if current.as_ref().is_some_and(|current| {
            current.source_instance_id == source_instance_id
                && current.selected == selection.selected
                && current.rollback == selection.rollback
        }) {
            continue;
        }
        let selection_epoch = current
            .map(|current| current.selection_epoch)
            .unwrap_or_default()
            .checked_add(1)
            .ok_or_else(|| {
                EngineError::InvalidCommit("query-pack selection epoch overflow".to_string())
            })?;
        statement
            .execute(params![
                to_i64(source_instance_id, "source instance id")?,
                selection.query_pack_id,
                selection.scope_key,
                selection.selected.query_id,
                i64::from(selection.selected.contract_version),
                selection.rollback.query_id,
                i64::from(selection.rollback.contract_version),
                to_i64(selection_epoch, "query-pack selection epoch")?,
                to_i64(commit_seq, "commit sequence")?,
                updated_at,
            ])
            .map_err(|error| sqlite_error("write query-pack selection", error))?;
    }
    Ok(())
}

fn write_record_errors(
    transaction: &Transaction<'_>,
    source_object_id: u64,
    commit_seq: u64,
    errors: &[SourceRecordError],
) -> Result<(), EngineError> {
    let mut statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO source_record_errors (
                source_object_id, generation, cursor_start, cursor_end,
                payload_hash, media_type, raw_payload, error_class,
                error_message, adapter_version, contract_version,
                first_commit_seq, last_retry_at, retry_count
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0)
            ON CONFLICT(source_object_id, generation, cursor_start, cursor_end) DO UPDATE SET
                payload_hash = excluded.payload_hash,
                media_type = excluded.media_type,
                raw_payload = excluded.raw_payload,
                error_class = excluded.error_class,
                error_message = excluded.error_message,
                adapter_version = excluded.adapter_version,
                contract_version = excluded.contract_version,
                last_retry_at = excluded.last_retry_at,
                retry_count = source_record_errors.retry_count + 1
            "#,
        )
        .map_err(|error| sqlite_error("prepare source record diagnostic", error))?;
    for error in errors {
        statement
            .execute(params![
                to_i64(source_object_id, "source object id")?,
                to_i64(error.generation, "record error generation")?,
                error.cursor_start,
                error.cursor_end,
                error.payload_hash,
                error.media_type,
                error.raw_payload,
                error.error_class,
                error.error_message,
                error.adapter_version,
                i64::from(error.contract_version),
                to_i64(commit_seq, "commit sequence")?,
                error.last_retry_at,
            ])
            .map_err(|error| sqlite_error("write source record diagnostic", error))?;
    }
    Ok(())
}

fn write_change_log(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    changes: &[ChangeEntry],
) -> Result<(), EngineError> {
    let mut retained_payload_bytes = 0_u64;
    let mut statement = transaction
        .prepare_cached(
            r#"
            INSERT INTO change_log (
                commit_seq, ordinal, topic, schema_version, entity_key,
                operation, payload
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .map_err(|error| sqlite_error("prepare durable change", error))?;
    for (ordinal, change) in changes.iter().enumerate() {
        retained_payload_bytes = retained_payload_bytes
            .checked_add(change_log_payload_bytes(change)?)
            .ok_or_else(|| {
                EngineError::InvalidCommit(
                    "change-log retained payload accounting overflowed u64".to_string(),
                )
            })?;
        statement
            .execute(params![
                to_i64(commit_seq, "commit sequence")?,
                i64::try_from(ordinal).map_err(|_| EngineError::InvalidCommit(
                    "change ordinal exceeds SQLite integer range".to_string()
                ))?,
                change.topic,
                i64::from(change.schema_version),
                change.entity_key,
                change.operation,
                change.payload,
            ])
            .map_err(|error| sqlite_error("write durable change", error))?;
    }
    drop(statement);
    if !changes.is_empty() {
        transaction
            .execute(
                r#"
                UPDATE change_log_retention_state
                SET retained_change_count = retained_change_count + ?1,
                    retained_payload_bytes = retained_payload_bytes + ?2
                WHERE singleton = 1
                "#,
                params![
                    i64::try_from(changes.len()).map_err(|_| EngineError::InvalidCommit(
                        "change count exceeds SQLite integer range".to_string()
                    ))?,
                    to_i64(retained_payload_bytes, "change-log retained payload bytes")?,
                ],
            )
            .map_err(|error| sqlite_error("account durable changes", error))?;
    }
    Ok(())
}

/// Persist writer-owned administrative changes through the same validation,
/// accounting, and ordinal rules as observation commits.
pub(super) fn write_internal_changes(
    transaction: &Transaction<'_>,
    commit_seq: u64,
    changes: &[ChangeEntry],
) -> Result<(), EngineError> {
    validate_changes(changes)?;
    write_change_log(transaction, commit_seq, changes)
}

fn change_log_payload_bytes(change: &ChangeEntry) -> Result<u64, EngineError> {
    [
        change.topic.len(),
        change.entity_key.len(),
        change.operation.len(),
        change.payload.len(),
    ]
    .into_iter()
    .try_fold(CHANGE_LOG_ROW_OVERHEAD_BYTES, |total, bytes| {
        let bytes = u64::try_from(bytes).map_err(|_| {
            EngineError::InvalidCommit(
                "change-log field length exceeds durable accounting range".to_string(),
            )
        })?;
        total.checked_add(bytes).ok_or_else(|| {
            EngineError::InvalidCommit(
                "change-log row payload accounting overflowed u64".to_string(),
            )
        })
    })
}

/// Prune only complete commit groups. The minimum sequence window wins over
/// both age and size, so a large recent commit can temporarily exceed the
/// byte target without making a just-issued cursor non-resumable.
pub(super) fn maintain_change_log(
    connection: &mut Connection,
    policy: ChangeLogRetentionPolicy,
    now_ms: i64,
) -> Result<ChangeLogRetentionSnapshot, EngineError> {
    validate_change_log_retention_policy(policy)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin change-log retention", error))?;
    let snapshot = prune_change_log(&transaction, policy, now_ms)?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit change-log retention", error))?;
    Ok(snapshot)
}

fn validate_change_log_retention_policy(
    policy: ChangeLogRetentionPolicy,
) -> Result<(), EngineError> {
    if policy.max_age_ms == 0 {
        return Err(EngineError::InvalidConfig(
            "change-log max_age_ms must be greater than zero".to_string(),
        ));
    }
    if policy.max_payload_bytes == 0 {
        return Err(EngineError::InvalidConfig(
            "change-log max_payload_bytes must be greater than zero".to_string(),
        ));
    }
    if policy.min_resumable_commits == 0 {
        return Err(EngineError::InvalidConfig(
            "change-log min_resumable_commits must be greater than zero".to_string(),
        ));
    }
    to_i64(policy.max_age_ms, "change-log maximum age")?;
    to_i64(policy.max_payload_bytes, "change-log maximum payload bytes")?;
    to_i64(
        policy.min_resumable_commits,
        "change-log minimum resumable commits",
    )?;
    Ok(())
}

fn prune_change_log(
    transaction: &Transaction<'_>,
    policy: ChangeLogRetentionPolicy,
    now_ms: i64,
) -> Result<ChangeLogRetentionSnapshot, EngineError> {
    validate_change_log_retention_policy(policy)?;
    let watermark = transaction
        .query_row(
            "SELECT COALESCE(MAX(commit_seq), 0) FROM ingest_commits WHERE committed_at IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_error("read retention watermark", error))?;
    let watermark = from_i64(watermark, "retention watermark")?;
    let protected_floor = watermark.saturating_sub(policy.min_resumable_commits);

    let (current_floor, retained_change_count, retained_payload_bytes): (i64, i64, i64) =
        transaction
            .query_row(
                r#"
                SELECT pruned_through_commit_seq, retained_change_count,
                       retained_payload_bytes
                FROM change_log_retention_state WHERE singleton = 1
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| sqlite_error("read change-log retention state", error))?;
    let current_floor = from_i64(current_floor, "change-log pruned floor")?;
    let retained_change_count = from_i64(retained_change_count, "retained change count")?;
    let retained_payload_bytes = from_i64(retained_payload_bytes, "retained change payload bytes")?;

    let max_age_ms = i64::try_from(policy.max_age_ms).map_err(|_| {
        EngineError::InvalidConfig("change-log maximum age exceeds i64".to_string())
    })?;
    let age_cutoff = now_ms.saturating_sub(max_age_ms);
    let age_candidate: i64 = transaction
        .query_row(
            r#"
            SELECT COALESCE(MAX(commit_seq), 0)
            FROM ingest_commits
            WHERE committed_at IS NOT NULL
              AND committed_at <= ?1
              AND commit_seq <= ?2
            "#,
            params![
                age_cutoff,
                to_i64(protected_floor, "retention protected floor")?
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("select age retention boundary", error))?;
    let age_candidate = from_i64(age_candidate, "age retention boundary")?;

    let size_candidate =
        if retained_payload_bytes > policy.max_payload_bytes && protected_floor > current_floor {
            let bytes_to_remove = retained_payload_bytes - policy.max_payload_bytes;
            transaction
                .query_row(
                    r#"
                WITH per_commit AS (
                  SELECT commit_seq,
                         SUM(
                           length(CAST(topic AS BLOB)) + length(entity_key) +
                           length(CAST(operation AS BLOB)) + length(payload) + ?1
                         ) AS payload_bytes
                  FROM change_log
                  WHERE commit_seq > ?2 AND commit_seq <= ?3
                  GROUP BY commit_seq
                ), cumulative AS (
                  SELECT commit_seq,
                         SUM(payload_bytes) OVER (
                           ORDER BY commit_seq ROWS UNBOUNDED PRECEDING
                         ) AS removed_bytes
                  FROM per_commit
                )
                SELECT COALESCE(
                  (SELECT MIN(commit_seq) FROM cumulative WHERE removed_bytes >= ?4),
                  (SELECT MAX(commit_seq) FROM per_commit),
                  0
                )
                "#,
                    params![
                        to_i64(CHANGE_LOG_ROW_OVERHEAD_BYTES, "change-log row overhead")?,
                        to_i64(current_floor, "change-log current floor")?,
                        to_i64(protected_floor, "retention protected floor")?,
                        to_i64(bytes_to_remove, "change-log bytes to remove")?,
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| sqlite_error("select size retention boundary", error))
                .and_then(|value| from_i64(value, "size retention boundary"))?
        } else {
            0
        };

    let next_floor = current_floor
        .max(age_candidate)
        .max(size_candidate)
        .min(protected_floor);
    if next_floor > current_floor {
        let (removed_count, removed_bytes): (i64, i64) = transaction
            .query_row(
                r#"
                SELECT COUNT(*), COALESCE(SUM(
                  length(CAST(topic AS BLOB)) + length(entity_key) +
                  length(CAST(operation AS BLOB)) + length(payload) + ?1
                ), 0)
                FROM change_log
                WHERE commit_seq <= ?2
                "#,
                params![
                    to_i64(CHANGE_LOG_ROW_OVERHEAD_BYTES, "change-log row overhead")?,
                    to_i64(next_floor, "next change-log floor")?,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| sqlite_error("measure pruned change log", error))?;
        let removed_count = from_i64(removed_count, "pruned change count")?;
        let removed_bytes = from_i64(removed_bytes, "pruned change payload bytes")?;
        transaction
            .execute(
                "DELETE FROM change_log WHERE commit_seq <= ?1",
                [to_i64(next_floor, "next change-log floor")?],
            )
            .map_err(|error| sqlite_error("prune change log", error))?;
        transaction
            .execute(
                r#"
                UPDATE change_log_retention_state
                SET pruned_through_commit_seq = ?1,
                    retained_change_count = ?2,
                    retained_payload_bytes = ?3,
                    last_pruned_at = ?4
                WHERE singleton = 1
                "#,
                params![
                    to_i64(next_floor, "next change-log floor")?,
                    to_i64(
                        retained_change_count.saturating_sub(removed_count),
                        "retained change count",
                    )?,
                    to_i64(
                        retained_payload_bytes.saturating_sub(removed_bytes),
                        "retained change payload bytes",
                    )?,
                    now_ms,
                ],
            )
            .map_err(|error| sqlite_error("advance change-log retention floor", error))?;
    } else {
        transaction
            .execute(
                "UPDATE change_log_retention_state SET last_pruned_at = ?1 WHERE singleton = 1",
                [now_ms],
            )
            .map_err(|error| sqlite_error("record change-log retention check", error))?;
    }

    read_change_log_retention_snapshot(transaction)
}

fn read_change_log_retention_snapshot(
    transaction: &Transaction<'_>,
) -> Result<ChangeLogRetentionSnapshot, EngineError> {
    let (floor, count, bytes): (i64, i64, i64) = transaction
        .query_row(
            r#"
            SELECT pruned_through_commit_seq, retained_change_count,
                   retained_payload_bytes
            FROM change_log_retention_state WHERE singleton = 1
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| sqlite_error("read retained change metrics", error))?;
    let oldest = transaction
        .query_row(
            "SELECT commit_seq, ordinal FROM change_log ORDER BY commit_seq, ordinal LIMIT 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| sqlite_error("read oldest retained change", error))?;
    let (oldest_retained_commit_seq, oldest_retained_ordinal) = match oldest {
        Some((commit_seq, ordinal)) => (
            Some(from_i64(commit_seq, "oldest retained commit")?),
            Some(u32::try_from(ordinal).map_err(|_| EngineError::Sqlite {
                operation: "decode oldest retained change",
                detail: format!("change ordinal was outside u32: {ordinal}"),
            })?),
        ),
        None => (None, None),
    };
    Ok(ChangeLogRetentionSnapshot {
        pruned_through_commit_seq: from_i64(floor, "change-log pruned floor")?,
        retained_change_count: from_i64(count, "retained change count")?,
        retained_payload_bytes: from_i64(bytes, "retained change payload bytes")?,
        oldest_retained_commit_seq,
        oldest_retained_ordinal,
    })
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} exceeds SQLite integer range")))
}

fn from_i64(value: i64, field: &'static str) -> Result<u64, EngineError> {
    u64::try_from(value).map_err(|_| EngineError::Sqlite {
        operation: "decode non-negative identifier",
        detail: format!("{field} was negative: {value}"),
    })
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
        CanonicalSourceInstanceKey, CoverageDeclarationDigest, CoverageDomain,
        CoverageMembershipRevision, CoverageScope, CoverageSetCompleteness, SourceCoverageSet,
    };
    use crate::core::schema;
    use crate::engine::{ChangeReplayRequest, EngineOptions, SpaghettiEngineCore};
    use tempfile::tempdir;

    struct FailAt(CommitStage);

    impl CommitHook for FailAt {
        fn reach(&self, stage: CommitStage) -> Result<(), EngineError> {
            if stage == self.0 {
                Err(EngineError::InjectedFailure {
                    stage: stage_name(stage),
                })
            } else {
                Ok(())
            }
        }
    }

    struct FailProjectionAt(ProjectionCommitStage);

    impl ProjectionCommitHook for FailProjectionAt {
        fn reach(&self, stage: ProjectionCommitStage) -> Result<(), EngineError> {
            if stage == self.0 {
                Err(EngineError::InjectedFailure {
                    stage: projection_stage_name(stage),
                })
            } else {
                Ok(())
            }
        }
    }

    struct FixtureProjectionWork;

    impl TransactionalProjectionWork for FixtureProjectionWork {
        fn apply_canonical(
            &self,
            transaction: &Transaction<'_>,
            context: &ProjectionCommitContext,
        ) -> Result<Vec<ChangeEntry>, EngineError> {
            write_fixture_projection(transaction, "fixture_canonical", context)?;
            Ok(Vec::new())
        }

        fn apply_runtime(
            &self,
            transaction: &Transaction<'_>,
            context: &ProjectionCommitContext,
        ) -> Result<Vec<ChangeEntry>, EngineError> {
            write_fixture_projection(transaction, "fixture_runtime", context)?;
            Ok(Vec::new())
        }

        fn apply_usage(
            &self,
            transaction: &Transaction<'_>,
            context: &ProjectionCommitContext,
        ) -> Result<Vec<ChangeEntry>, EngineError> {
            write_fixture_projection(transaction, "fixture_usage", context)?;
            Ok(Vec::new())
        }
    }

    fn write_fixture_projection(
        transaction: &Transaction<'_>,
        table: &'static str,
        context: &ProjectionCommitContext,
    ) -> Result<(), EngineError> {
        let sql = match table {
            "fixture_canonical" => "INSERT INTO fixture_canonical VALUES (?1, ?2, ?3, ?4, ?5)",
            "fixture_runtime" => "INSERT INTO fixture_runtime VALUES (?1, ?2, ?3, ?4, ?5)",
            "fixture_usage" => "INSERT INTO fixture_usage VALUES (?1, ?2, ?3, ?4, ?5)",
            _ => unreachable!(),
        };
        transaction
            .execute(
                sql,
                params![
                    to_i64(context.commit_seq, "fixture commit sequence")?,
                    to_i64(context.source_instance_id, "fixture source instance")?,
                    to_i64(context.source_stream_id, "fixture source stream")?,
                    to_i64(context.source_object_id, "fixture source object")?,
                    to_i64(context.generation, "fixture generation")?,
                ],
            )
            .map(|_| ())
            .map_err(|error| sqlite_error("write fixture projection", error))
    }

    fn stage_name(stage: CommitStage) -> &'static str {
        match stage {
            CommitStage::BeforeTransaction => "before transaction",
            CommitStage::MidCanonicalProjection => "mid canonical projection",
            CommitStage::MidRuntimeProjection => "mid runtime projection",
            CommitStage::MidUsageProjection => "mid usage projection",
            CommitStage::AfterCursorUpdate => "after cursor update",
            CommitStage::AfterOutboxInsert => "after outbox insert",
            CommitStage::BeforeCommit => "before commit",
            CommitStage::AfterCommit => "after commit",
            CommitStage::BeforePublish => "before in-memory publish",
        }
    }

    fn projection_stage_name(stage: ProjectionCommitStage) -> &'static str {
        match stage {
            ProjectionCommitStage::BeforeTransaction => "before projection transaction",
            ProjectionCommitStage::AfterCommitInsert => "after projection commit insert",
            ProjectionCommitStage::AfterProjectionVersions => "after projection version writes",
            ProjectionCommitStage::AfterCoverageReplacement => {
                "after projection coverage replacement"
            }
            ProjectionCommitStage::AfterQueryPackSelections => "after query-pack selection writes",
            ProjectionCommitStage::BeforeCommit => "before projection commit",
            ProjectionCommitStage::AfterCommit => "after projection commit",
        }
    }

    fn initialize_fixture_tables(connection: &Connection) {
        connection
            .execute_batch(
                r#"
                CREATE TABLE fixture_canonical(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_runtime(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_usage(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                "#,
            )
            .unwrap();
    }

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        initialize_fixture_tables(&connection);
        connection
    }

    fn request() -> ObservationCommit {
        ObservationCommit {
            source: SourceInstanceSpec {
                adapter_id: "fixture".to_string(),
                stable_key: b"fixture-root".to_vec(),
                display_name: "Fixture root".to_string(),
                adapter_version: "1.0.0".to_string(),
                adapter_contract_version: 3,
                source_schema_versions: Vec::new(),
                capabilities: Vec::new(),
                discovered_at: 1_000,
                last_seen_at: 1_100,
            },
            stream: SourceStreamSpec {
                stream_key: "transcripts".to_string(),
                driver_kind: "append_file".to_string(),
                decoder_key: "fixture.jsonl".to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: Some(1_050),
                consistency: ConsistencyPolicy::IncrementalCursor,
                retention: RawRetentionPolicy::Full,
            },
            object: SourceObjectUpdate {
                object_key: b"session-1".to_vec(),
                expected: ExpectedSourceCursor::Absent,
                display_path: Some("sessions/1.jsonl".to_string()),
                native_identity: Some(b"inode:1".to_vec()),
                generation: 1,
                committed_cursor: b"byte:128".to_vec(),
                observed_revision: Some(b"rev:1".to_vec()),
                adapter_object_context: Some(b"context".to_vec()),
                driver_checkpoint: Some(b"append-checkpoint".to_vec()),
                driver_checkpoint_version: Some(1),
                decoder_state: Some(b"decoder".to_vec()),
                decoder_state_version: Some(2),
                retry_state: None,
                size_bytes: Some(128),
                mtime_ns: Some(1_000_000),
                decoder_contract_version: 4,
                state: "active".to_string(),
            },
            reason: "live_append".to_string(),
            started_at: 1_100,
            committed_at: 1_200,
            fact_count: 2,
            projection_versions: vec![ProjectionVersionUpdate {
                projection_id: "canonical.history".to_string(),
                scope_key: b"fixture-root".to_vec(),
                desired_version: 5,
                completed_version: Some(5),
                readiness: ProjectionReadiness::Ready,
                detail: None,
            }],
            record_errors: vec![SourceRecordError {
                generation: 1,
                cursor_start: b"byte:64".to_vec(),
                cursor_end: b"byte:80".to_vec(),
                payload_hash: b"sha256:fixture".to_vec(),
                media_type: "application/x-ndjson".to_string(),
                raw_payload: None,
                error_class: "unknown_record".to_string(),
                error_message: "future fixture record".to_string(),
                adapter_version: "1.0.0".to_string(),
                contract_version: 3,
                last_retry_at: None,
            }],
            changes: vec![
                ChangeEntry {
                    topic: "history.session.changed".to_string(),
                    schema_version: 1,
                    entity_key: b"session-1".to_vec(),
                    operation: "upsert".to_string(),
                    payload: br#"{"session":"session-1"}"#.to_vec(),
                },
                ChangeEntry {
                    topic: "runtime.session.changed".to_string(),
                    schema_version: 1,
                    entity_key: b"session-1".to_vec(),
                    operation: "upsert".to_string(),
                    payload: br#"{"state":"active"}"#.to_vec(),
                },
            ],
        }
    }

    fn usage_v2_projection_commit(source_instance_id: u64) -> ProjectionVersionCommit {
        let domain = CoverageDomain::FactFamily {
            family: "runtime.usage-v2".to_string(),
            version: 1,
        };
        let source_instance_key = CanonicalSourceInstanceKey::derive(1, b"fixture-root").unwrap();
        let coverage = SourceCoverageSet::new(
            domain,
            CoverageScope {
                adapter_id: "fixture".to_string(),
                source_instance_key,
                root_entity_key: None,
                support_release_id: "fixture-support-release".to_string(),
                source_or_scope_declaration_digest: CoverageDeclarationDigest::derive(
                    b"fixture-source-declaration",
                )
                .unwrap(),
            },
            CoverageMembershipRevision::derive(b"fixture-empty-membership").unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .unwrap();
        ProjectionVersionCommit {
            source_instance_id,
            reason: "projection.runtime.usage-v2.ready".to_string(),
            started_at: 1_300,
            committed_at: 1_301,
            projection_versions: vec![ProjectionVersionUpdate {
                projection_id: "runtime.usage-v2".to_string(),
                scope_key: b"fixture-root".to_vec(),
                desired_version: 1,
                completed_version: Some(1),
                readiness: ProjectionReadiness::Ready,
                detail: None,
            }],
            coverage_sets: vec![DurableCoverageSetUpdate {
                owner_id: "runtime.usage-v2".to_string(),
                owner_scope_key: b"fixture-root".to_vec(),
                set: coverage,
            }],
            coverage_preconditions: Vec::new(),
            query_pack_selections: Vec::new(),
        }
    }

    fn query_value(query_id: &str) -> QueryPackSelectionValue {
        QueryPackSelectionValue {
            query_id: query_id.to_string(),
            contract_version: 1,
        }
    }

    fn usage_v2_selection_commit(
        source_instance_id: u64,
        expected: QueryPackSelectionExpectation,
        selected_query_id: &str,
        rollback_query_id: &str,
        guarded: bool,
    ) -> ProjectionVersionCommit {
        ProjectionVersionCommit {
            source_instance_id,
            reason: format!("query.runtime.usage.select.{selected_query_id}"),
            started_at: 1_400,
            committed_at: 1_401,
            projection_versions: Vec::new(),
            coverage_sets: Vec::new(),
            coverage_preconditions: Vec::new(),
            query_pack_selections: vec![QueryPackSelectionUpdate {
                query_pack_id: "runtime.usage".to_string(),
                scope_key: b"fixture-root".to_vec(),
                expected,
                selected: query_value(selected_query_id),
                rollback: query_value(rollback_query_id),
                projection_guard: guarded.then(|| QueryPackProjectionGuard {
                    projection_id: "runtime.usage-v2".to_string(),
                    projection_scope_key: b"fixture-root".to_vec(),
                    projection_version: 1,
                    coverage_owner_id: "runtime.usage-v2".to_string(),
                    coverage_domain_name: "runtime.usage-v2".to_string(),
                    coverage_domain_version: 1,
                }),
            }],
        }
    }

    fn count(connection: &Connection, table: &'static str) -> i64 {
        let sql = match table {
            "source_instances" => "SELECT COUNT(*) FROM source_instances",
            "source_objects" => "SELECT COUNT(*) FROM source_objects",
            "ingest_commits" => "SELECT COUNT(*) FROM ingest_commits",
            "projection_versions" => "SELECT COUNT(*) FROM projection_versions",
            "query_pack_selections" => "SELECT COUNT(*) FROM query_pack_selections",
            "source_coverage_sets" => "SELECT COUNT(*) FROM source_coverage_sets",
            "source_record_errors" => "SELECT COUNT(*) FROM source_record_errors",
            "change_log" => "SELECT COUNT(*) FROM change_log",
            "fixture_canonical" => "SELECT COUNT(*) FROM fixture_canonical",
            "fixture_runtime" => "SELECT COUNT(*) FROM fixture_runtime",
            "fixture_usage" => "SELECT COUNT(*) FROM fixture_usage",
            _ => unreachable!(),
        };
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn commit_atomically_persists_catalog_cursor_projection_diagnostics_and_outbox() {
        let mut connection = database();
        let receipt = apply_observation_commit_with_components(
            &mut connection,
            &request(),
            &FixtureProjectionWork,
            &NoopCommitHook,
        )
        .unwrap();

        assert_eq!(receipt.commit_seq, 1);
        assert_eq!(receipt.change_count, 2);
        assert_eq!(count(&connection, "source_instances"), 1);
        assert_eq!(count(&connection, "source_objects"), 1);
        assert_eq!(count(&connection, "ingest_commits"), 1);
        assert_eq!(count(&connection, "projection_versions"), 1);
        assert_eq!(count(&connection, "source_record_errors"), 1);
        assert_eq!(count(&connection, "change_log"), 2);
        assert_eq!(count(&connection, "fixture_canonical"), 1);
        assert_eq!(count(&connection, "fixture_runtime"), 1);
        assert_eq!(count(&connection, "fixture_usage"), 1);
        let raw_retention: String = connection
            .query_row("SELECT raw_retention FROM source_streams", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(raw_retention, "full");
        let provenance: (i64, i64, i64, i64, i64) = connection
            .query_row(
                r#"
                SELECT commit_seq, source_instance_id, source_stream_id,
                       source_object_id, generation
                FROM fixture_canonical
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
        assert_eq!(
            provenance,
            (
                1,
                to_i64(receipt.source_instance_id, "test source instance").unwrap(),
                to_i64(receipt.source_stream_id, "test source stream").unwrap(),
                to_i64(receipt.source_object_id, "test source object").unwrap(),
                1,
            )
        );

        let commit: (i64, i64) = connection
            .query_row(
                "SELECT committed_at, fact_count FROM ingest_commits WHERE commit_seq = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(commit, (1_200, 2));
        let cursor: (i64, Vec<u8>, i64) = connection
            .query_row(
                "SELECT generation, committed_cursor, last_commit_seq FROM source_objects",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(cursor, (1, b"byte:128".to_vec(), 1));
        let opaque_state: (Vec<u8>, i64, Vec<u8>, i64) = connection
            .query_row(
                r#"
                SELECT driver_checkpoint, driver_checkpoint_version,
                       decoder_state, decoder_state_version
                FROM source_objects
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            opaque_state,
            (b"append-checkpoint".to_vec(), 1, b"decoder".to_vec(), 2)
        );
        let projection: (i64, String, i64) = connection
            .query_row(
                "SELECT completed_version, readiness, last_commit_seq FROM projection_versions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(projection, (5, "ready".to_string(), 1));
    }

    #[test]
    fn projection_readiness_advances_the_commit_clock_without_source_cursor_churn() {
        let mut connection = database();
        let source = apply_observation_commit(&mut connection, &request()).unwrap();
        let pending = ProjectionVersionUpdate {
            projection_id: "runtime.usage-v2".to_string(),
            scope_key: b"fixture-root".to_vec(),
            desired_version: 1,
            completed_version: None,
            readiness: ProjectionReadiness::Pending,
            detail: Some("source reconciliation in progress".to_string()),
        };
        let pending_request = ProjectionVersionCommit {
            source_instance_id: source.source_instance_id,
            reason: "projection.runtime.usage-v2.pending".to_string(),
            started_at: 1_300,
            committed_at: 1_301,
            projection_versions: vec![pending.clone()],
            coverage_sets: Vec::new(),
            coverage_preconditions: Vec::new(),
            query_pack_selections: Vec::new(),
        };
        let receipt = apply_projection_version_commit(&mut connection, &pending_request)
            .unwrap()
            .expect("first transition advances the commit clock");
        assert_eq!(receipt.commit_seq, 2);
        assert_eq!(count(&connection, "ingest_commits"), 2);
        assert_eq!(count(&connection, "source_objects"), 1);
        assert_eq!(count(&connection, "change_log"), 2);
        assert!(
            apply_projection_version_commit(&mut connection, &pending_request)
                .unwrap()
                .is_none(),
            "an equal transition must not churn the durable watermark"
        );
        assert_eq!(count(&connection, "ingest_commits"), 2);

        let ready_request = ProjectionVersionCommit {
            source_instance_id: source.source_instance_id,
            reason: "projection.runtime.usage-v2.ready".to_string(),
            started_at: 1_400,
            committed_at: 1_401,
            projection_versions: vec![ProjectionVersionUpdate {
                completed_version: Some(1),
                readiness: ProjectionReadiness::Ready,
                detail: None,
                ..pending
            }],
            coverage_sets: Vec::new(),
            coverage_preconditions: Vec::new(),
            query_pack_selections: Vec::new(),
        };
        let ready = apply_projection_version_commit(&mut connection, &ready_request)
            .unwrap()
            .expect("readiness transition advances the commit clock");
        assert_eq!(ready.commit_seq, 3);
        assert_eq!(
            connection
                .query_row(
                    "SELECT desired_version, completed_version, readiness, last_commit_seq, detail FROM projection_versions WHERE projection_id = 'runtime.usage-v2'",
                    [],
                    |row| Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    )),
                )
                .unwrap(),
            (1, 1, "ready".to_string(), 3, None)
        );
    }

    #[test]
    fn query_pack_selection_requires_ready_complete_coverage_and_rolls_back_explicitly() {
        let mut connection = database();
        let source = apply_observation_commit(&mut connection, &request()).unwrap();
        let promotion = usage_v2_selection_commit(
            source.source_instance_id,
            QueryPackSelectionExpectation::Absent,
            "runtime.usage-v2",
            "legacy.usage",
            true,
        );

        assert!(matches!(
            apply_projection_version_commit(&mut connection, &promotion),
            Err(EngineError::InvalidCommit(detail)) if detail.contains("guard")
        ));
        assert_eq!(count(&connection, "ingest_commits"), 1);
        assert_eq!(count(&connection, "query_pack_selections"), 0);

        apply_projection_version_commit(
            &mut connection,
            &usage_v2_projection_commit(source.source_instance_id),
        )
        .unwrap()
        .expect("ready coverage barrier advances the commit clock");
        let selected = apply_projection_version_commit(&mut connection, &promotion)
            .unwrap()
            .expect("ready complete usage-v2 may be selected");
        assert_eq!(selected.commit_seq, 3);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT selected_query_id, selected_contract_version,
                           rollback_query_id, rollback_contract_version,
                           selection_epoch, last_commit_seq, source_instance_id
                    FROM query_pack_selections
                    WHERE query_pack_id = 'runtime.usage'
                    "#,
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .unwrap(),
            (
                "runtime.usage-v2".to_string(),
                1,
                "legacy.usage".to_string(),
                1,
                1,
                3,
                to_i64(source.source_instance_id, "test source instance").unwrap(),
            )
        );
        assert!(
            apply_projection_version_commit(&mut connection, &promotion)
                .unwrap()
                .is_none(),
            "acknowledgement retry must accept its already-selected target"
        );
        assert_eq!(count(&connection, "ingest_commits"), 3);

        let stale_rollback = usage_v2_selection_commit(
            source.source_instance_id,
            QueryPackSelectionExpectation::At {
                selected: query_value("runtime.usage-v2"),
                selection_epoch: 99,
            },
            "legacy.usage",
            "runtime.usage-v2",
            false,
        );
        assert!(matches!(
            apply_projection_version_commit(&mut connection, &stale_rollback),
            Err(EngineError::InvalidCommit(detail)) if detail.contains("compare-and-set")
        ));
        assert_eq!(count(&connection, "ingest_commits"), 3);

        let pending = ProjectionVersionCommit {
            source_instance_id: source.source_instance_id,
            reason: "projection.runtime.usage-v2.pending".to_string(),
            started_at: 1_500,
            committed_at: 1_501,
            projection_versions: vec![ProjectionVersionUpdate {
                projection_id: "runtime.usage-v2".to_string(),
                scope_key: b"fixture-root".to_vec(),
                desired_version: 1,
                completed_version: Some(1),
                readiness: ProjectionReadiness::Pending,
                detail: Some("new source evidence requires reduction".to_string()),
            }],
            coverage_sets: Vec::new(),
            coverage_preconditions: Vec::new(),
            query_pack_selections: Vec::new(),
        };
        apply_projection_version_commit(&mut connection, &pending)
            .unwrap()
            .expect("provider work makes the selected projection pending");

        let rollback = usage_v2_selection_commit(
            source.source_instance_id,
            QueryPackSelectionExpectation::At {
                selected: query_value("runtime.usage-v2"),
                selection_epoch: 1,
            },
            "legacy.usage",
            "legacy.usage",
            false,
        );
        let rolled_back = apply_projection_version_commit(&mut connection, &rollback)
            .unwrap()
            .expect("rollback must not require a healthy v2 projection");
        assert_eq!(rolled_back.commit_seq, 5);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT selected_query_id, rollback_query_id, selection_epoch,
                           last_commit_seq
                    FROM query_pack_selections
                    WHERE query_pack_id = 'runtime.usage'
                    "#,
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .unwrap(),
            ("legacy.usage".to_string(), "legacy.usage".to_string(), 2, 5,)
        );
        assert_eq!(count(&connection, "source_coverage_sets"), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT readiness FROM projection_versions WHERE projection_id = 'runtime.usage-v2'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "pending"
        );
    }

    #[test]
    fn query_pack_selection_crash_boundaries_are_atomic_and_ack_retry_is_idempotent() {
        let precommit_stages = [
            ProjectionCommitStage::BeforeTransaction,
            ProjectionCommitStage::AfterCommitInsert,
            ProjectionCommitStage::AfterProjectionVersions,
            ProjectionCommitStage::AfterCoverageReplacement,
            ProjectionCommitStage::AfterQueryPackSelections,
            ProjectionCommitStage::BeforeCommit,
        ];
        for stage in precommit_stages {
            let mut connection = database();
            let source = apply_observation_commit(&mut connection, &request()).unwrap();
            apply_projection_version_commit(
                &mut connection,
                &usage_v2_projection_commit(source.source_instance_id),
            )
            .unwrap();
            let promotion = usage_v2_selection_commit(
                source.source_instance_id,
                QueryPackSelectionExpectation::Absent,
                "runtime.usage-v2",
                "legacy.usage",
                true,
            );
            assert!(matches!(
                apply_projection_version_commit_with_hook(
                    &mut connection,
                    &promotion,
                    &FailProjectionAt(stage),
                ),
                Err(EngineError::InjectedFailure { .. })
            ));
            assert_eq!(count(&connection, "ingest_commits"), 2, "{stage:?}");
            assert_eq!(count(&connection, "query_pack_selections"), 0, "{stage:?}");
            let receipt = apply_projection_version_commit(&mut connection, &promotion)
                .unwrap()
                .expect("retry after a precommit failure applies once");
            assert_eq!(receipt.commit_seq, 3, "{stage:?}");
        }

        let mut connection = database();
        let source = apply_observation_commit(&mut connection, &request()).unwrap();
        apply_projection_version_commit(
            &mut connection,
            &usage_v2_projection_commit(source.source_instance_id),
        )
        .unwrap();
        let promotion = usage_v2_selection_commit(
            source.source_instance_id,
            QueryPackSelectionExpectation::Absent,
            "runtime.usage-v2",
            "legacy.usage",
            true,
        );
        assert!(matches!(
            apply_projection_version_commit_with_hook(
                &mut connection,
                &promotion,
                &FailProjectionAt(ProjectionCommitStage::AfterCommit),
            ),
            Err(EngineError::InjectedFailure { .. })
        ));
        assert_eq!(count(&connection, "ingest_commits"), 3);
        assert_eq!(count(&connection, "query_pack_selections"), 1);
        assert!(
            apply_projection_version_commit(&mut connection, &promotion)
                .unwrap()
                .is_none(),
            "retry after acknowledgement loss must observe the durable target"
        );
        assert_eq!(count(&connection, "ingest_commits"), 3);
    }

    #[test]
    fn projection_and_coverage_precommit_failure_seams_roll_back_together() {
        let stages = [
            ProjectionCommitStage::BeforeTransaction,
            ProjectionCommitStage::AfterCommitInsert,
            ProjectionCommitStage::AfterProjectionVersions,
            ProjectionCommitStage::AfterCoverageReplacement,
            ProjectionCommitStage::AfterQueryPackSelections,
            ProjectionCommitStage::BeforeCommit,
        ];

        for stage in stages {
            let mut connection = database();
            let source = apply_observation_commit(&mut connection, &request()).unwrap();
            let migration = usage_v2_projection_commit(source.source_instance_id);
            let result = apply_projection_version_commit_with_hook(
                &mut connection,
                &migration,
                &FailProjectionAt(stage),
            );
            assert!(
                matches!(result, Err(EngineError::InjectedFailure { .. })),
                "stage {stage:?} returned {result:?}"
            );
            assert_eq!(count(&connection, "ingest_commits"), 1, "{stage:?}");
            assert_eq!(
                count(&connection, "source_coverage_sets"),
                0,
                "{stage:?} leaked coverage"
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM projection_versions WHERE projection_id = 'runtime.usage-v2'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "{stage:?} leaked readiness"
            );

            let receipt = apply_projection_version_commit(&mut connection, &migration)
                .unwrap()
                .expect("retry after a precommit crash must apply once");
            assert_eq!(receipt.commit_seq, 2, "{stage:?}");
            assert_eq!(count(&connection, "ingest_commits"), 2, "{stage:?}");
            assert_eq!(count(&connection, "source_coverage_sets"), 1, "{stage:?}");
            assert_eq!(
                connection
                    .query_row(
                        r#"
                        SELECT projection.readiness,
                               projection.last_commit_seq,
                               coverage.completeness,
                               coverage.last_commit_seq
                        FROM projection_versions AS projection
                        JOIN source_coverage_sets AS coverage
                          ON coverage.owner_id = projection.projection_id
                         AND coverage.owner_scope_key = projection.scope_key
                        WHERE projection.projection_id = 'runtime.usage-v2'
                        "#,
                        [],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, i64>(3)?,
                            ))
                        },
                    )
                    .unwrap(),
                ("ready".to_string(), 2, "complete".to_string(), 2),
                "{stage:?} did not converge atomically"
            );
        }
    }

    #[test]
    fn projection_postcommit_ack_loss_is_restart_idempotent() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("projection-postcommit-restart.db");
        let source_instance_id;

        {
            let mut connection = Connection::open(&database_path).unwrap();
            schema::initialize_schema(&connection).unwrap();
            initialize_fixture_tables(&connection);
            source_instance_id = apply_observation_commit(&mut connection, &request())
                .unwrap()
                .source_instance_id;
            let migration = usage_v2_projection_commit(source_instance_id);
            let result = apply_projection_version_commit_with_hook(
                &mut connection,
                &migration,
                &FailProjectionAt(ProjectionCommitStage::AfterCommit),
            );
            assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
            assert_eq!(count(&connection, "ingest_commits"), 2);
            assert_eq!(count(&connection, "source_coverage_sets"), 1);
        }

        let mut connection = Connection::open(&database_path).unwrap();
        schema::initialize_schema(&connection).unwrap();
        let migration = usage_v2_projection_commit(source_instance_id);
        assert!(
            apply_projection_version_commit(&mut connection, &migration)
                .unwrap()
                .is_none(),
            "retry after restart must recognize the durable transition"
        );
        assert_eq!(count(&connection, "ingest_commits"), 2);
        assert_eq!(count(&connection, "source_coverage_sets"), 1);
        assert_eq!(
            connection
                .query_row(
                    r#"
                    SELECT projection.last_commit_seq, coverage.last_commit_seq
                    FROM projection_versions AS projection
                    JOIN source_coverage_sets AS coverage
                      ON coverage.owner_id = projection.projection_id
                     AND coverage.owner_scope_key = projection.scope_key
                    WHERE projection.projection_id = 'runtime.usage-v2'
                    "#,
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (2, 2)
        );
    }

    #[test]
    fn cursor_compare_and_swap_makes_committed_range_retry_idempotent() {
        let mut connection = database();
        let original = request();
        let first = apply_observation_commit(&mut connection, &original).unwrap();

        assert!(matches!(
            apply_observation_commit(&mut connection, &original),
            Err(EngineError::StaleSourceCursor { .. })
        ));
        assert_eq!(count(&connection, "ingest_commits"), 1);
        assert_eq!(count(&connection, "change_log"), 2);

        let mut next = original;
        next.object.expected = ExpectedSourceCursor::At {
            generation: 1,
            committed_cursor: b"byte:128".to_vec(),
        };
        next.object.committed_cursor = b"byte:256".to_vec();
        next.started_at = 1_300;
        next.committed_at = 1_400;
        let receipt = apply_observation_commit(&mut connection, &next).unwrap();
        assert_eq!(receipt.commit_seq, 2);
        assert_eq!(receipt.source_instance_id, first.source_instance_id);
        assert_eq!(receipt.source_stream_id, first.source_stream_id);
        assert_eq!(receipt.source_object_id, first.source_object_id);
        assert_eq!(count(&connection, "ingest_commits"), 2);
    }

    #[test]
    fn unchanged_source_registration_does_not_rewrite_the_hot_catalog_row() {
        let mut connection = database();
        let first = request();
        apply_observation_commit(&mut connection, &first).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TEMP TABLE source_instance_updates (marker INTEGER);
                CREATE TEMP TRIGGER observe_source_instance_update
                AFTER UPDATE ON source_instances BEGIN
                  INSERT INTO source_instance_updates VALUES (1);
                END;
                "#,
            )
            .unwrap();

        let mut second = first.clone();
        second.object.expected = ExpectedSourceCursor::At {
            generation: 1,
            committed_cursor: b"byte:128".to_vec(),
        };
        second.object.committed_cursor = b"byte:256".to_vec();
        second.started_at += 100;
        second.committed_at += 100;
        apply_observation_commit(&mut connection, &second).unwrap();
        let update_count = |connection: &Connection| {
            connection
                .query_row("SELECT COUNT(*) FROM source_instance_updates", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        assert_eq!(update_count(&connection), 0);

        second.object.expected = ExpectedSourceCursor::At {
            generation: 1,
            committed_cursor: b"byte:256".to_vec(),
        };
        second.object.committed_cursor = b"byte:384".to_vec();
        second.source.last_seen_at += 1;
        second.started_at += 100;
        second.committed_at += 100;
        apply_observation_commit(&mut connection, &second).unwrap();
        assert_eq!(update_count(&connection), 1);
    }

    #[test]
    fn automatic_retention_runs_on_a_bounded_commit_cadence() {
        let mut connection = database();
        let mut next = request();

        let first_maintenance_at =
            2_000 + i64::try_from(AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS).unwrap();
        for commit_seq in 1..=3 * AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
            let cursor = format!("byte:{}", commit_seq * 128).into_bytes();
            if commit_seq > 1 {
                next.object.expected = ExpectedSourceCursor::At {
                    generation: 1,
                    committed_cursor: format!("byte:{}", (commit_seq - 1) * 128).into_bytes(),
                };
            }
            next.object.committed_cursor = cursor;
            next.started_at = 1_000 + i64::try_from(commit_seq).unwrap();
            next.committed_at =
                if commit_seq <= 2 * AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
                    2_000 + i64::try_from(commit_seq).unwrap()
                } else {
                    first_maintenance_at
                        + AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_MS
                        + i64::try_from(commit_seq).unwrap()
                };
            apply_observation_commit(&mut connection, &next).unwrap();

            let last_pruned_at: Option<i64> = connection
                .query_row(
                    "SELECT last_pruned_at FROM change_log_retention_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if commit_seq < AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
                assert_eq!(last_pruned_at, None);
            } else if commit_seq < 3 * AUTOMATIC_CHANGE_LOG_MAINTENANCE_INTERVAL_COMMITS {
                assert_eq!(last_pruned_at, Some(first_maintenance_at));
            } else {
                assert_eq!(last_pruned_at, Some(next.committed_at));
            }
        }
    }

    #[test]
    fn catalog_identity_is_independent_of_unrelated_registration_order() {
        fn commit_named(connection: &mut Connection, stable_key: &[u8]) -> CommitReceipt {
            let mut request = request();
            request.source.stable_key = stable_key.to_vec();
            request.source.display_name = String::from_utf8_lossy(stable_key).into_owned();
            request.object.object_key = b"shared-object-name".to_vec();
            apply_observation_commit(connection, &request).unwrap()
        }

        let mut first_order = database();
        let alpha_first = commit_named(&mut first_order, b"alpha-root");
        let beta_second = commit_named(&mut first_order, b"beta-root");

        let mut reverse_order = database();
        let beta_first = commit_named(&mut reverse_order, b"beta-root");
        let alpha_second = commit_named(&mut reverse_order, b"alpha-root");

        assert_eq!(
            alpha_first.source_instance_id,
            alpha_second.source_instance_id
        );
        assert_eq!(alpha_first.source_stream_id, alpha_second.source_stream_id);
        assert_eq!(alpha_first.source_object_id, alpha_second.source_object_id);
        assert_eq!(
            beta_second.source_instance_id,
            beta_first.source_instance_id
        );
        assert_eq!(beta_second.source_stream_id, beta_first.source_stream_id);
        assert_eq!(beta_second.source_object_id, beta_first.source_object_id);
        assert_ne!(
            alpha_first.source_instance_id,
            beta_first.source_instance_id
        );
    }

    #[test]
    fn driver_checkpoint_requires_a_nonzero_paired_version_before_writes() {
        for (checkpoint, version) in [
            (Some(b"checkpoint".to_vec()), None),
            (None, Some(1)),
            (Some(b"checkpoint".to_vec()), Some(0)),
            (Some(vec![0; MAX_DRIVER_CHECKPOINT_BYTES + 1]), Some(1)),
        ] {
            let mut connection = database();
            let mut invalid = request();
            invalid.object.driver_checkpoint = checkpoint;
            invalid.object.driver_checkpoint_version = version;
            assert!(matches!(
                apply_observation_commit(&mut connection, &invalid),
                Err(EngineError::InvalidCommit(_))
            ));
            assert_eq!(count(&connection, "source_instances"), 0);
            assert_eq!(count(&connection, "source_objects"), 0);
            assert_eq!(count(&connection, "ingest_commits"), 0);
        }
    }

    #[test]
    fn every_precommit_failure_seam_rolls_back_all_visible_effects() {
        let stages = [
            CommitStage::BeforeTransaction,
            CommitStage::MidCanonicalProjection,
            CommitStage::MidRuntimeProjection,
            CommitStage::MidUsageProjection,
            CommitStage::AfterCursorUpdate,
            CommitStage::AfterOutboxInsert,
            CommitStage::BeforeCommit,
        ];

        for stage in stages {
            let mut connection = database();
            let result = apply_observation_commit_with_components(
                &mut connection,
                &request(),
                &FixtureProjectionWork,
                &FailAt(stage),
            );
            assert!(
                matches!(result, Err(EngineError::InjectedFailure { .. })),
                "stage {stage:?} returned {result:?}"
            );
            for table in [
                "source_instances",
                "source_objects",
                "ingest_commits",
                "projection_versions",
                "source_record_errors",
                "change_log",
                "fixture_canonical",
                "fixture_runtime",
                "fixture_usage",
            ] {
                assert_eq!(count(&connection, table), 0, "{stage:?} leaked {table}");
            }
        }
    }

    #[test]
    fn postcommit_failure_is_recoverable_and_retry_has_no_duplicate_effect() {
        for stage in [CommitStage::AfterCommit, CommitStage::BeforePublish] {
            let mut connection = database();
            let original = request();
            let result = apply_observation_commit_with_components(
                &mut connection,
                &original,
                &FixtureProjectionWork,
                &FailAt(stage),
            );
            assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
            assert_eq!(count(&connection, "ingest_commits"), 1);
            assert_eq!(count(&connection, "source_objects"), 1);
            assert_eq!(count(&connection, "projection_versions"), 1);
            assert_eq!(count(&connection, "change_log"), 2);
            assert_eq!(count(&connection, "fixture_canonical"), 1);
            assert_eq!(count(&connection, "fixture_runtime"), 1);
            assert_eq!(count(&connection, "fixture_usage"), 1);

            assert!(matches!(
                apply_observation_commit(&mut connection, &original),
                Err(EngineError::StaleSourceCursor { .. })
            ));
            assert_eq!(count(&connection, "ingest_commits"), 1);
            assert_eq!(count(&connection, "change_log"), 2);
        }
    }

    #[test]
    fn restart_replays_an_outbox_committed_before_publication() {
        let directory = tempdir().unwrap();
        let database_path = directory.path().join("postcommit-replay.db");
        let mut connection = Connection::open(&database_path).unwrap();
        schema::set_pragmas(&connection).unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE fixture_canonical(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_runtime(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                CREATE TABLE fixture_usage(
                  commit_seq INTEGER PRIMARY KEY, source_instance_id INTEGER,
                  source_stream_id INTEGER, source_object_id INTEGER, generation INTEGER
                );
                "#,
            )
            .unwrap();

        let result = apply_observation_commit_with_components(
            &mut connection,
            &request(),
            &FixtureProjectionWork,
            &FailAt(CommitStage::BeforePublish),
        );
        assert!(matches!(result, Err(EngineError::InjectedFailure { .. })));
        drop(connection);

        let engine = SpaghettiEngineCore::open(EngineOptions {
            database_path,
            query_workers: Some(1),
            owner_label: Some("postcommit-restart-test".to_string()),
            defer_query_structures: false,
            source_pass_pool: None,
        })
        .unwrap();
        let replay = engine
            .replay_changes(ChangeReplayRequest {
                after: None,
                topics: Vec::new(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(replay.at_commit_seq, 1);
        assert_eq!(replay.changes.len(), 2);
        assert_eq!(replay.changes[0].payload, br#"{"session":"session-1"}"#);
        assert_eq!(replay.changes[1].payload, br#"{"state":"active"}"#);
        engine.shutdown().unwrap();
    }
}
