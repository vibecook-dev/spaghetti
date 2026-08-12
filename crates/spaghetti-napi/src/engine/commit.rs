//! RFC 011 atomic source-cursor commit coordinator.
//!
//! This module owns the transaction boundary shared by catalog state,
//! projection readiness, record diagnostics, and the durable change log. The
//! adapter-specific fact/projector implementations land in later phases and
//! must execute inside this boundary before the cursor is advanced.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::EngineError;

const MAX_DRIVER_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;

/// Stable schema version for a change payload. This is independent from the
/// SQLite schema version and is supplied by the projector that owns a topic.
pub type ChangeSchemaVersion = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInstanceSpec {
    pub adapter_id: String,
    pub stable_key: Vec<u8>,
    pub display_name: String,
    pub adapter_contract_version: u32,
    pub discovered_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStreamSpec {
    pub stream_key: String,
    pub driver_kind: String,
    pub decoder_key: String,
    pub stream_state: String,
    pub last_reconciled_at: Option<i64>,
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

pub(super) trait CommitHook: Send + Sync {
    fn reach(&self, stage: CommitStage) -> Result<(), EngineError>;
}

struct NoopCommitHook;

impl CommitHook for NoopCommitHook {
    fn reach(&self, _stage: CommitStage) -> Result<(), EngineError> {
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
    apply_observation_commit_with_components(
        connection,
        request,
        &NoProjectionWork,
        &NoopCommitHook,
    )
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

pub(super) fn apply_observation_commit_with_projection(
    connection: &mut Connection,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
) -> Result<CommitReceipt, EngineError> {
    apply_observation_commit_with_components(connection, request, projection_work, &NoopCommitHook)
}

pub(super) fn apply_observation_commit_with_components(
    connection: &mut Connection,
    request: &ObservationCommit,
    projection_work: &dyn TransactionalProjectionWork,
    hook: &dyn CommitHook,
) -> Result<CommitReceipt, EngineError> {
    validate_commit(request)?;
    hook.reach(CommitStage::BeforeTransaction)?;

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin ingest commit", error))?;

    let source_instance_id = upsert_source_instance(&transaction, &request.source)?;
    let commit_seq = insert_ingest_commit(&transaction, source_instance_id, request)?;
    let source_stream_id = upsert_source_stream(
        &transaction,
        source_instance_id,
        commit_seq,
        &request.stream,
    )?;
    let existing = read_source_object(&transaction, source_stream_id, &request.object.object_key)?;
    verify_cursor_precondition(request, existing.as_ref())?;
    let source_object_id = reserve_source_object_identity(
        &transaction,
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
    };

    let mut changes = request.changes.clone();
    changes.extend(projection_work.apply_canonical(&transaction, &projection_context)?);
    hook.reach(CommitStage::MidCanonicalProjection)?;
    changes.extend(projection_work.apply_runtime(&transaction, &projection_context)?);
    hook.reach(CommitStage::MidRuntimeProjection)?;
    changes.extend(projection_work.apply_usage(&transaction, &projection_context)?);
    hook.reach(CommitStage::MidUsageProjection)?;
    let changes = coalesce_changes(changes);
    validate_changes(&changes)?;

    finalize_source_object(&transaction, source_object_id, commit_seq, &request.object)?;
    hook.reach(CommitStage::AfterCursorUpdate)?;

    write_projection_versions(
        &transaction,
        commit_seq,
        request.committed_at,
        &request.projection_versions,
    )?;
    write_record_errors(
        &transaction,
        source_object_id,
        commit_seq,
        &request.record_errors,
    )?;
    write_change_log(&transaction, commit_seq, &changes)?;
    hook.reach(CommitStage::AfterOutboxInsert)?;

    transaction
        .execute(
            "UPDATE ingest_commits SET committed_at = ?1, fact_count = ?2 WHERE commit_seq = ?3",
            params![
                request.committed_at,
                i64::from(request.fact_count),
                to_i64(commit_seq, "commit sequence")?
            ],
        )
        .map_err(|error| sqlite_error("finalize ingest commit", error))?;
    hook.reach(CommitStage::BeforeCommit)?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit ingest transaction", error))?;

    hook.reach(CommitStage::AfterCommit)?;
    hook.reach(CommitStage::BeforePublish)?;

    Ok(CommitReceipt {
        commit_seq,
        source_instance_id,
        source_stream_id,
        source_object_id,
        change_count: u32::try_from(changes.len()).unwrap_or(u32::MAX),
    })
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

    for projection in &request.projection_versions {
        require_text("projection.projection_id", &projection.projection_id)?;
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

fn validate_source_instance(source: &SourceInstanceSpec) -> Result<(), EngineError> {
    require_text("source.adapter_id", &source.adapter_id)?;
    require_bytes("source.stable_key", &source.stable_key)?;
    require_text("source.display_name", &source.display_name)?;
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
    let id: i64 = transaction
        .query_row(
            r#"
            INSERT INTO source_instances (
                adapter_id, stable_key, display_name, adapter_contract_version,
                discovered_at, last_seen_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(adapter_id, stable_key) DO UPDATE SET
                display_name = excluded.display_name,
                adapter_contract_version = excluded.adapter_contract_version,
                last_seen_at = excluded.last_seen_at
            RETURNING source_instance_id
            "#,
            params![
                source.adapter_id,
                source.stable_key,
                source.display_name,
                i64::from(source.adapter_contract_version),
                source.discovered_at,
                source.last_seen_at
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("upsert source instance", error))?;
    from_i64(id, "source instance id")
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
            ) VALUES (?1, ?2, ?3, NULL, 0)
            RETURNING commit_seq
            "#,
            params![
                to_i64(source_instance_id, "source instance id")?,
                request.reason,
                request.started_at
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
    let id: i64 = transaction
        .query_row(
            r#"
            INSERT INTO source_streams (
                source_instance_id, stream_key, driver_kind, decoder_key,
                stream_state, last_reconciled_at, last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(source_instance_id, stream_key) DO UPDATE SET
                driver_kind = excluded.driver_kind,
                decoder_key = excluded.decoder_key,
                stream_state = excluded.stream_state,
                last_reconciled_at = excluded.last_reconciled_at,
                last_commit_seq = excluded.last_commit_seq
            RETURNING source_stream_id
            "#,
            params![
                to_i64(source_instance_id, "source instance id")?,
                stream.stream_key,
                stream.driver_kind,
                stream.decoder_key,
                stream.stream_state,
                stream.last_reconciled_at,
                to_i64(commit_seq, "commit sequence")?
            ],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("upsert source stream", error))?;
    from_i64(id, "source stream id")
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

    let id: i64 = transaction
        .query_row(
            r#"
            INSERT INTO source_objects (
                source_stream_id, object_key, generation, committed_cursor,
                decoder_contract_version, state
            ) VALUES (?1, ?2, ?3, X'', ?4, 'pending')
            RETURNING source_object_id
            "#,
            params![
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
                decoder_state_version = ?10, size_bytes = ?11, mtime_ns = ?12,
                decoder_contract_version = ?13, last_commit_seq = ?14,
                state = ?15
            WHERE source_object_id = ?16
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
    Ok(())
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

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
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
        connection
    }

    fn request() -> ObservationCommit {
        ObservationCommit {
            source: SourceInstanceSpec {
                adapter_id: "fixture".to_string(),
                stable_key: b"fixture-root".to_vec(),
                display_name: "Fixture root".to_string(),
                adapter_contract_version: 3,
                discovered_at: 1_000,
                last_seen_at: 1_100,
            },
            stream: SourceStreamSpec {
                stream_key: "transcripts".to_string(),
                driver_kind: "append_file".to_string(),
                decoder_key: "fixture.jsonl".to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: Some(1_050),
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

    fn count(connection: &Connection, table: &'static str) -> i64 {
        let sql = match table {
            "source_instances" => "SELECT COUNT(*) FROM source_instances",
            "source_objects" => "SELECT COUNT(*) FROM source_objects",
            "ingest_commits" => "SELECT COUNT(*) FROM ingest_commits",
            "projection_versions" => "SELECT COUNT(*) FROM projection_versions",
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
        assert_eq!(provenance, (1, 1, 1, 1, 1));

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
    fn cursor_compare_and_swap_makes_committed_range_retry_idempotent() {
        let mut connection = database();
        let original = request();
        apply_observation_commit(&mut connection, &original).unwrap();

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
        assert_eq!(receipt.source_instance_id, 1);
        assert_eq!(receipt.source_stream_id, 1);
        assert_eq!(receipt.source_object_id, 1);
        assert_eq!(count(&connection, "ingest_commits"), 2);
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
