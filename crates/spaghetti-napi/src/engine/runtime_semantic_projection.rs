//! RFC 012C topology-neutral actor and affiliation projection.
//!
//! This component consumes only common facts. It neither knows adapter IDs
//! nor joins legacy catalog-local run, workflow, or team keys. Usage remains
//! one response table and consumers regroup it through these current rows.

use rusqlite::{params, Transaction};

use crate::adapter::{
    ActorAffiliationDimension, ActorAffiliationState, ActorRunRole, ConsistencyPolicy, Fact,
    FactBatch, FactEnvelope, FactRevisionId, QualifiedTimestamp, TimestampQuality,
};

use super::commit::ProjectionCommitContext;
use super::EngineError;

/// Common adapter capability and durable projection-pack identifier. Concrete
/// adapters may declare it, but only the common reducer/query stack interprets
/// its readiness lifecycle.
pub(super) const USAGE_V2_PROJECTION_ID: &str = "runtime.usage-v2";
pub(super) const USAGE_V2_PROJECTION_VERSION: u32 = 1;

pub(super) fn apply_runtime_semantic_v2_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<(), EngineError> {
    let has_affiliation_fact = batch
        .facts()
        .iter()
        .any(|envelope| matches!(envelope.value, Fact::ActorAffiliationRevision(_)));
    let owns_affiliation_snapshot = context.consistency == ConsistencyPolicy::SnapshotReplace
        && !context.skip_unowned_replace_document(has_affiliation_fact);
    if !owns_affiliation_snapshot && context.replaces_prior_generation {
        transaction
            .execute(
                "DELETE FROM runtime_actor_affiliations_v2 WHERE source_object_id = ?1 AND source_generation <> ?2",
                params![
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.generation, "source generation")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced actor affiliations", error))?;
    }
    if context.replaces_prior_generation {
        transaction
            .execute(
                "DELETE FROM runtime_actor_runs_v2 WHERE source_object_id = ?1 AND source_generation <> ?2",
                params![
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.generation, "source generation")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced actor runs", error))?;
    }

    for envelope in batch.facts() {
        match &envelope.value {
            Fact::ActorRunRevision(fact) => {
                fact.validate().map_err(|error| {
                    EngineError::InvalidCommit(format!("invalid actor-run fact: {error}"))
                })?;
                let semantic = required_semantic_revision(envelope, "actor-run")?;
                require_value_semantic_revision(
                    semantic,
                    &fact.semantic_revision_key().map_err(|error| {
                        EngineError::InvalidCommit(format!(
                            "invalid actor-run semantic revision: {error}"
                        ))
                    })?,
                    "actor-run",
                )?;
                let affected = transaction
                    .execute(
                        r#"
                        INSERT INTO runtime_actor_runs_v2 (
                            actor_run_key, semantic_fact_id, fact_revision_id,
                            source_record_id, session_key, role,
                            parent_actor_run_key, native_session_id,
                            native_actor_id, native_actor_type, fact_id,
                            source_object_id, source_generation, cursor_end,
                            last_commit_seq
                        ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                            ?11, ?12, ?13, ?14, ?15
                        )
                        ON CONFLICT(actor_run_key) DO UPDATE SET
                            semantic_fact_id = excluded.semantic_fact_id,
                            fact_revision_id = excluded.fact_revision_id,
                            source_record_id = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_runs_v2.fact_revision_id
                              THEN runtime_actor_runs_v2.source_record_id
                              ELSE excluded.source_record_id
                            END,
                            session_key = excluded.session_key,
                            role = excluded.role,
                            parent_actor_run_key = excluded.parent_actor_run_key,
                            native_session_id = excluded.native_session_id,
                            native_actor_id = excluded.native_actor_id,
                            native_actor_type = excluded.native_actor_type,
                            fact_id = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_runs_v2.fact_revision_id
                              THEN runtime_actor_runs_v2.fact_id
                              ELSE excluded.fact_id
                            END,
                            source_object_id = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_runs_v2.fact_revision_id
                              THEN runtime_actor_runs_v2.source_object_id
                              ELSE excluded.source_object_id
                            END,
                            source_generation = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_runs_v2.fact_revision_id
                              THEN runtime_actor_runs_v2.source_generation
                              ELSE excluded.source_generation
                            END,
                            cursor_end = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_runs_v2.fact_revision_id
                              THEN runtime_actor_runs_v2.cursor_end
                              ELSE excluded.cursor_end
                            END,
                            last_commit_seq = excluded.last_commit_seq
                        WHERE excluded.semantic_fact_id = runtime_actor_runs_v2.semantic_fact_id
                          AND excluded.session_key = runtime_actor_runs_v2.session_key
                          AND excluded.role = runtime_actor_runs_v2.role
                          AND excluded.source_object_id = runtime_actor_runs_v2.source_object_id
                          AND (
                            excluded.source_generation > runtime_actor_runs_v2.source_generation
                            OR (
                              excluded.source_generation = runtime_actor_runs_v2.source_generation
                              AND excluded.cursor_end >= runtime_actor_runs_v2.cursor_end
                            )
                          )
                          AND (
                            excluded.fact_revision_id <> runtime_actor_runs_v2.fact_revision_id
                            OR excluded.source_generation = runtime_actor_runs_v2.source_generation
                          )
                        "#,
                        params![
                            fact.actor_run.as_bytes().as_slice(),
                            semantic.fact_id.as_bytes().as_slice(),
                            semantic.fact_revision_id.as_bytes().as_slice(),
                            semantic.source_record_id.as_bytes().as_slice(),
                            fact.session.as_bytes().as_slice(),
                            actor_run_role(fact.role),
                            fact.parent_actor_run
                                .as_ref()
                                .map(|key| key.as_bytes().as_slice()),
                            fact.native_session_id,
                            fact.native_actor_id,
                            fact.native_actor_type,
                            envelope.id.as_bytes().as_slice(),
                            sqlite_u64(context.source_object_id, "source object id")?,
                            sqlite_u64(context.generation, "source generation")?,
                            envelope.provenance.cursor_end,
                            sqlite_u64(context.commit_seq, "commit sequence")?,
                        ],
                    )
                    .map_err(|error| sqlite_error("write actor run", error))?;
                record_rejected_revision(transaction, context, envelope, affected, "actor-run")?;
            }
            Fact::ActorAffiliationRevision(fact) => {
                fact.validate().map_err(|error| {
                    EngineError::InvalidCommit(format!("invalid actor-affiliation fact: {error}"))
                })?;
                let semantic = required_semantic_revision(envelope, "actor-affiliation")?;
                require_value_semantic_revision(
                    semantic,
                    &fact.semantic_revision_key().map_err(|error| {
                        EngineError::InvalidCommit(format!(
                            "invalid actor-affiliation semantic revision: {error}"
                        ))
                    })?,
                    "actor-affiliation",
                )?;
                let affected = transaction
                    .execute(
                        r#"
                        INSERT INTO runtime_actor_affiliations_v2 (
                            affiliation_key, semantic_fact_id, fact_revision_id,
                            source_record_id, actor_run_key, session_key,
                            dimension, target_key, member_key, native_target_id,
                            native_member_id, state, effective_at,
                            effective_at_quality, fact_id, source_object_id,
                            source_generation, cursor_end, last_commit_seq
                        ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                            ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19
                        )
                        ON CONFLICT(affiliation_key) DO UPDATE SET
                            semantic_fact_id = excluded.semantic_fact_id,
                            fact_revision_id = excluded.fact_revision_id,
                            source_record_id = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_affiliations_v2.fact_revision_id
                              THEN runtime_actor_affiliations_v2.source_record_id
                              ELSE excluded.source_record_id
                            END,
                            actor_run_key = excluded.actor_run_key,
                            session_key = excluded.session_key,
                            dimension = excluded.dimension,
                            target_key = excluded.target_key,
                            member_key = excluded.member_key,
                            native_target_id = excluded.native_target_id,
                            native_member_id = excluded.native_member_id,
                            state = excluded.state,
                            effective_at = excluded.effective_at,
                            effective_at_quality = excluded.effective_at_quality,
                            fact_id = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_affiliations_v2.fact_revision_id
                              THEN runtime_actor_affiliations_v2.fact_id
                              ELSE excluded.fact_id
                            END,
                            source_object_id = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_affiliations_v2.fact_revision_id
                              THEN runtime_actor_affiliations_v2.source_object_id
                              ELSE excluded.source_object_id
                            END,
                            source_generation = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_affiliations_v2.fact_revision_id
                              THEN runtime_actor_affiliations_v2.source_generation
                              ELSE excluded.source_generation
                            END,
                            cursor_end = CASE
                              WHEN excluded.fact_revision_id = runtime_actor_affiliations_v2.fact_revision_id
                              THEN runtime_actor_affiliations_v2.cursor_end
                              ELSE excluded.cursor_end
                            END,
                            last_commit_seq = excluded.last_commit_seq
                        WHERE excluded.semantic_fact_id = runtime_actor_affiliations_v2.semantic_fact_id
                          AND excluded.actor_run_key = runtime_actor_affiliations_v2.actor_run_key
                          AND excluded.session_key = runtime_actor_affiliations_v2.session_key
                          AND excluded.dimension = runtime_actor_affiliations_v2.dimension
                          AND excluded.target_key = runtime_actor_affiliations_v2.target_key
                          AND excluded.source_object_id = runtime_actor_affiliations_v2.source_object_id
                          AND (
                            excluded.source_generation > runtime_actor_affiliations_v2.source_generation
                            OR (
                              excluded.source_generation = runtime_actor_affiliations_v2.source_generation
                              AND excluded.cursor_end >= runtime_actor_affiliations_v2.cursor_end
                            )
                          )
                          AND (
                            excluded.fact_revision_id <> runtime_actor_affiliations_v2.fact_revision_id
                            OR excluded.source_generation = runtime_actor_affiliations_v2.source_generation
                          )
                        "#,
                        params![
                            fact.affiliation.as_bytes().as_slice(),
                            semantic.fact_id.as_bytes().as_slice(),
                            semantic.fact_revision_id.as_bytes().as_slice(),
                            semantic.source_record_id.as_bytes().as_slice(),
                            fact.actor_run.as_bytes().as_slice(),
                            fact.session.as_bytes().as_slice(),
                            affiliation_dimension(fact.dimension),
                            fact.target.as_bytes().as_slice(),
                            fact.member
                                .as_ref()
                                .map(|key| key.as_bytes().as_slice()),
                            fact.native_target_id,
                            fact.native_member_id,
                            affiliation_state(fact.state),
                            timestamp_value(fact.effective_at.as_ref()),
                            timestamp_quality(fact.effective_at.as_ref()),
                            envelope.id.as_bytes().as_slice(),
                            sqlite_u64(context.source_object_id, "source object id")?,
                            sqlite_u64(context.generation, "source generation")?,
                            envelope.provenance.cursor_end,
                            sqlite_u64(context.commit_seq, "commit sequence")?,
                        ],
                    )
                    .map_err(|error| sqlite_error("write actor affiliation", error))?;
                record_rejected_revision(
                    transaction,
                    context,
                    envelope,
                    affected,
                    "actor-affiliation",
                )?;
            }
            _ => {}
        }
    }
    if owns_affiliation_snapshot {
        transaction
            .execute(
                r#"
                DELETE FROM fact_records
                WHERE source_object_id = ?1
                  AND fact_kind = 'runtime.actor-affiliation'
                  AND last_commit_seq <> ?2
                "#,
                params![
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.commit_seq, "commit sequence")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced actor affiliation facts", error))?;
    }
    Ok(())
}

fn required_semantic_revision<'a>(
    envelope: &'a FactEnvelope,
    fact_name: &str,
) -> Result<&'a crate::adapter::FactSemanticRevision, EngineError> {
    let semantic = envelope.semantic_revision.as_ref().ok_or_else(|| {
        EngineError::InvalidCommit(format!(
            "{fact_name} fact is missing its mandatory semantic revision"
        ))
    })?;
    if semantic.semantic_revision_ref.fact_revision_id != semantic.fact_revision_id {
        return Err(EngineError::InvalidCommit(format!(
            "{fact_name} semantic reference does not match its fact revision"
        )));
    }
    Ok(semantic)
}

fn require_value_semantic_revision(
    semantic: &crate::adapter::FactSemanticRevision,
    revision_key: &[u8],
    fact_name: &str,
) -> Result<(), EngineError> {
    let expected = FactRevisionId::derive(&semantic.fact_id, 1, revision_key).map_err(|error| {
        EngineError::InvalidCommit(format!(
            "invalid {fact_name} semantic revision identity: {error}"
        ))
    })?;
    if semantic.fact_revision_id != expected {
        return Err(EngineError::InvalidCommit(format!(
            "{fact_name} semantic revision does not match its normalized value"
        )));
    }
    Ok(())
}

/// A revision the acceptance guard turned away: the accepted row is pinned to
/// a different member object of the same session, or the incoming record sits
/// behind the accepted cursor after a rewrite. Both are properties of a live
/// corpus — a session whose actor runs are asserted by more than one member
/// object is normal for multi-agent sessions — so losing the race must not
/// fail the commit: that turned one such record into a permanent startup
/// crash loop (2026-08-24 playground). Keep the accepted row, record the
/// rejection where per-record loss already lives, and continue.
/// A usage-v2 upsert the acceptance guard turned away. An identical accepted
/// revision is a replay no-op; anything else lost the acceptance race and is
/// recorded, never fatal — the same live-corpus hazard as the actor-run guard.
pub(super) fn handle_rejected_usage_revision(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    semantic: &crate::adapter::FactSemanticRevision,
) -> Result<(), EngineError> {
    use rusqlite::OptionalExtension as _;
    let accepted_revision = transaction
        .query_row(
            "SELECT fact_revision_id FROM usage_v2_response_contributions WHERE usage_key = ?1",
            [semantic.fact_id.as_bytes().as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|error| sqlite_error("read rejected usage-v2 revision", error))?;
    if accepted_revision.as_deref() == Some(semantic.fact_revision_id.as_bytes().as_slice()) {
        return Ok(());
    }
    record_rejected_revision(transaction, context, envelope, 0, "usage-v2")
}

pub(super) fn record_rejected_revision(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    affected: usize,
    fact_name: &str,
) -> Result<(), EngineError> {
    if affected == 1 {
        return Ok(());
    }
    transaction
        .execute(
            r#"
            INSERT INTO source_record_errors (
                source_object_id, generation, cursor_start, cursor_end,
                payload_hash, media_type, raw_payload, error_class,
                error_message, adapter_version, contract_version,
                first_commit_seq, last_retry_at, retry_count
            ) VALUES (?1, ?2, ?3, ?4, ?5, 'application/x-spaghetti-fact', NULL,
                      'revision_rejected', ?6, 'engine', 0, ?7, NULL, 0)
            ON CONFLICT(source_object_id, generation, cursor_start, cursor_end) DO UPDATE SET
                error_class = excluded.error_class,
                error_message = excluded.error_message,
                retry_count = source_record_errors.retry_count + 1
            "#,
            params![
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_start,
                envelope.provenance.cursor_end,
                envelope.provenance.record_hash.as_slice(),
                format!(
                    "{fact_name} revision rejected: the accepted row is pinned to a \
                     different source object or a later source cursor"
                ),
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("record rejected semantic revision", error))?;
    Ok(())
}

fn actor_run_role(role: ActorRunRole) -> &'static str {
    match role {
        ActorRunRole::Root => "root",
        ActorRunRole::Child => "child",
    }
}

fn affiliation_dimension(dimension: ActorAffiliationDimension) -> &'static str {
    match dimension {
        ActorAffiliationDimension::Team => "team",
        ActorAffiliationDimension::Workflow => "workflow",
    }
}

fn affiliation_state(state: ActorAffiliationState) -> &'static str {
    match state {
        ActorAffiliationState::Present => "present",
        ActorAffiliationState::Removed => "removed",
        ActorAffiliationState::Unknown => "unknown",
    }
}

fn timestamp_value(timestamp: Option<&QualifiedTimestamp>) -> Option<&str> {
    timestamp.map(|timestamp| timestamp.value.as_str())
}

fn timestamp_quality(timestamp: Option<&QualifiedTimestamp>) -> Option<&'static str> {
    timestamp.map(|timestamp| match timestamp.quality {
        TimestampQuality::NativeExact => "native_exact",
        TimestampQuality::NativeApproximate => "native_approximate",
        TimestampQuality::FileMetadataFallback => "file_metadata_fallback",
        TimestampQuality::Derived => "derived",
    })
}

fn sqlite_u64(value: u64, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} exceeds SQLite integer range")))
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
