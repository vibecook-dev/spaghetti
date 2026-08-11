//! Common RFC 011 typed-fact projectors.
//!
//! This is the only boundary that translates adapter facts into storage.
//! Adapters never receive a SQLite handle, table name, or change-log topic.

use std::collections::BTreeSet;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::adapter::{
    EntityKey, EvidenceKind, EvidenceStrength, Fact, FactBatch, FactEnvelope, MessageRole,
    QualifiedTimestamp, TimestampQuality, TokenUsage, UsageAccounting, UsageFact, UsageScope,
    ValueQuality,
};

use super::commit::{
    apply_observation_commit_with_projection, ChangeEntry, CommitReceipt, ObservationCommit,
    ProjectionCommitContext, TransactionalProjectionWork,
};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

/// Submit one already-decoded fact batch through the catalog/projection/cursor
/// transaction. Public changes and the durable fact count are derived here;
/// callers cannot supply adapter-owned event topics for typed commits.
pub(super) fn apply_fact_observation_commit(
    connection: &mut Connection,
    request: &ObservationCommit,
    batch: &FactBatch,
) -> Result<CommitReceipt, EngineError> {
    if !request.changes.is_empty() {
        return Err(EngineError::InvalidCommit(
            "typed fact commits cannot supply public changes".to_string(),
        ));
    }
    let fact_count = u32::try_from(batch.facts().len()).map_err(|_| {
        EngineError::InvalidCommit("fact batch exceeds durable count range".to_string())
    })?;
    if let Some(last) = batch.facts().last() {
        if last.provenance.cursor_end != request.object.committed_cursor {
            return Err(EngineError::InvalidCommit(
                "typed fact batch does not end at the committed source cursor".to_string(),
            ));
        }
    }
    let mut request = request.clone();
    request.fact_count = fact_count;
    if let Some(next_decoder_state) = batch.next_decoder_state() {
        if request.object.decoder_state_version.is_none() {
            return Err(EngineError::InvalidCommit(
                "next decoder state requires a decoder state version".to_string(),
            ));
        }
        request.object.decoder_state = Some(next_decoder_state.to_vec());
    }
    let projection = FactProjectionWork { batch };
    apply_observation_commit_with_projection(connection, &request, &projection)
}

struct FactProjectionWork<'a> {
    batch: &'a FactBatch,
}

impl TransactionalProjectionWork for FactProjectionWork<'_> {
    fn apply_canonical(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        validate_batch_provenance(self.batch, context)?;
        let mut changes = retract_canonical_generation(transaction, context)?;
        for envelope in self.batch.facts() {
            persist_fact(transaction, context, envelope)?;
            match &envelope.value {
                Fact::Session(fact) => {
                    transaction
                        .execute(
                            r#"
                            INSERT INTO canonical_sessions (
                                session_key, project_key, native_session_id,
                                native_project_key, cwd, git_branch, first_prompt,
                                ai_title, custom_title, source_time,
                                source_time_quality, fact_id, source_object_id,
                                source_generation, cursor_end, last_commit_seq
                            ) VALUES (
                                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                                ?11, ?12, ?13, ?14, ?15, ?16
                            )
                            ON CONFLICT(session_key) DO UPDATE SET
                                project_key = excluded.project_key,
                                native_session_id = excluded.native_session_id,
                                native_project_key = excluded.native_project_key,
                                cwd = COALESCE(excluded.cwd, canonical_sessions.cwd),
                                git_branch = COALESCE(excluded.git_branch, canonical_sessions.git_branch),
                                first_prompt = CASE
                                  WHEN excluded.first_prompt IS NOT NULL AND (
                                    canonical_sessions.first_prompt IS NULL
                                    OR trim(canonical_sessions.first_prompt) = ''
                                    OR canonical_sessions.first_prompt = 'No prompt'
                                  ) THEN excluded.first_prompt
                                  ELSE canonical_sessions.first_prompt
                                END,
                                ai_title = COALESCE(excluded.ai_title, canonical_sessions.ai_title),
                                custom_title = COALESCE(excluded.custom_title, canonical_sessions.custom_title),
                                source_time = COALESCE(excluded.source_time, canonical_sessions.source_time),
                                source_time_quality = COALESCE(excluded.source_time_quality, canonical_sessions.source_time_quality),
                                fact_id = excluded.fact_id,
                                source_object_id = excluded.source_object_id,
                                source_generation = excluded.source_generation,
                                cursor_end = excluded.cursor_end,
                                last_commit_seq = excluded.last_commit_seq
                            "#,
                            params![
                                fact.session.as_bytes(),
                                fact.project.as_bytes(),
                                fact.native_session_id,
                                fact.native_project_key,
                                fact.cwd,
                                fact.git_branch,
                                fact.first_prompt,
                                fact.ai_title,
                                fact.custom_title,
                                timestamp_value(fact.source_time.as_ref()),
                                timestamp_quality(fact.source_time.as_ref()),
                                envelope.id.as_bytes().as_slice(),
                                sqlite_u64(context.source_object_id, "source object id")?,
                                sqlite_u64(context.generation, "source generation")?,
                                envelope.provenance.cursor_end,
                                sqlite_u64(context.commit_seq, "commit sequence")?,
                            ],
                        )
                        .map_err(|error| sqlite_error("project canonical session", error))?;
                    changes.push(upsert_change(
                        "history.session.changed",
                        fact.session.as_bytes(),
                        envelope,
                    )?);
                }
                Fact::Message(fact) => {
                    let content = serialize(&fact.content, "serialize canonical content")?;
                    transaction
                        .execute(
                            r#"
                            INSERT INTO canonical_messages (
                                message_key, session_key, native_message_id,
                                native_kind, role, content_json, source_time,
                                source_time_quality, parent_native_message_id,
                                model, search_text, raw_json, fact_id,
                                source_object_id, source_generation, cursor_start,
                                cursor_end, last_commit_seq
                            ) VALUES (
                                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
                            )
                            ON CONFLICT(message_key) DO UPDATE SET
                                session_key = excluded.session_key,
                                native_message_id = excluded.native_message_id,
                                native_kind = excluded.native_kind,
                                role = excluded.role,
                                content_json = excluded.content_json,
                                source_time = excluded.source_time,
                                source_time_quality = excluded.source_time_quality,
                                parent_native_message_id = excluded.parent_native_message_id,
                                model = excluded.model,
                                search_text = excluded.search_text,
                                raw_json = excluded.raw_json,
                                fact_id = excluded.fact_id,
                                source_object_id = excluded.source_object_id,
                                source_generation = excluded.source_generation,
                                cursor_start = excluded.cursor_start,
                                cursor_end = excluded.cursor_end,
                                last_commit_seq = excluded.last_commit_seq
                            "#,
                            params![
                                fact.message.as_bytes(),
                                fact.session.as_bytes(),
                                fact.native_message_id,
                                fact.native_kind,
                                message_role(&fact.role),
                                content,
                                timestamp_value(fact.source_time.as_ref()),
                                timestamp_quality(fact.source_time.as_ref()),
                                fact.parent_native_message_id,
                                fact.model,
                                fact.search_text,
                                fact.raw_json,
                                envelope.id.as_bytes().as_slice(),
                                sqlite_u64(context.source_object_id, "source object id")?,
                                sqlite_u64(context.generation, "source generation")?,
                                envelope.provenance.cursor_start,
                                envelope.provenance.cursor_end,
                                sqlite_u64(context.commit_seq, "commit sequence")?,
                            ],
                        )
                        .map_err(|error| sqlite_error("project canonical message", error))?;
                    changes.push(upsert_change(
                        "history.message.changed",
                        fact.message.as_bytes(),
                        envelope,
                    )?);
                }
                Fact::Run(fact) => {
                    transaction
                        .execute(
                            r#"
                            INSERT INTO canonical_runs (
                                run_key, session_key, native_run_id,
                                parent_run_key, fact_id, source_object_id,
                                source_generation, cursor_end, last_commit_seq
                            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                            ON CONFLICT(run_key) DO UPDATE SET
                                session_key = excluded.session_key,
                                native_run_id = excluded.native_run_id,
                                parent_run_key = excluded.parent_run_key,
                                fact_id = excluded.fact_id,
                                source_object_id = excluded.source_object_id,
                                source_generation = excluded.source_generation,
                                cursor_end = excluded.cursor_end,
                                last_commit_seq = excluded.last_commit_seq
                            "#,
                            params![
                                fact.run.as_bytes(),
                                fact.session.as_bytes(),
                                fact.native_run_id,
                                fact.parent_run.as_ref().map(EntityKey::as_bytes),
                                envelope.id.as_bytes().as_slice(),
                                sqlite_u64(context.source_object_id, "source object id")?,
                                sqlite_u64(context.generation, "source generation")?,
                                envelope.provenance.cursor_end,
                                sqlite_u64(context.commit_seq, "commit sequence")?,
                            ],
                        )
                        .map_err(|error| sqlite_error("project canonical run", error))?;
                    changes.push(upsert_change(
                        "history.run.changed",
                        fact.run.as_bytes(),
                        envelope,
                    )?);
                }
                Fact::UnknownRecord { .. } => changes.push(upsert_change(
                    "diagnostic.source-record.preserved",
                    envelope.id.as_bytes(),
                    envelope,
                )?),
                Fact::RunEvidence(_) | Fact::Usage(_) => {}
            }
        }
        Ok(changes)
    }

    fn apply_runtime(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        let mut affected = old_generation_keys(
            transaction,
            "SELECT DISTINCT run_key FROM run_evidence WHERE source_object_id = ?1 AND source_generation <> ?2",
            context,
            "read replaced run evidence",
        )?;
        transaction
            .execute(
                "DELETE FROM run_evidence WHERE source_object_id = ?1 AND source_generation <> ?2",
                params![
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.generation, "source generation")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced run evidence", error))?;

        for envelope in self.batch.facts() {
            let Fact::RunEvidence(fact) = &envelope.value else {
                continue;
            };
            transaction
                .execute(
                    r#"
                    INSERT INTO run_evidence (
                        fact_id, run_key, evidence_kind, evidence_strength,
                        native_state, source_time, source_time_quality,
                        source_object_id, source_generation, cursor_end,
                        last_commit_seq
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                    ON CONFLICT(fact_id) DO UPDATE SET
                        run_key = excluded.run_key,
                        evidence_kind = excluded.evidence_kind,
                        evidence_strength = excluded.evidence_strength,
                        native_state = excluded.native_state,
                        source_time = excluded.source_time,
                        source_time_quality = excluded.source_time_quality,
                        source_object_id = excluded.source_object_id,
                        source_generation = excluded.source_generation,
                        cursor_end = excluded.cursor_end,
                        last_commit_seq = excluded.last_commit_seq
                    "#,
                    params![
                        envelope.id.as_bytes().as_slice(),
                        fact.run.as_bytes(),
                        evidence_kind(fact.kind),
                        evidence_strength(fact.strength),
                        fact.native_state,
                        timestamp_value(fact.source_time.as_ref()),
                        timestamp_quality(fact.source_time.as_ref()),
                        sqlite_u64(context.source_object_id, "source object id")?,
                        sqlite_u64(context.generation, "source generation")?,
                        envelope.provenance.cursor_end,
                        sqlite_u64(context.commit_seq, "commit sequence")?,
                    ],
                )
                .map_err(|error| sqlite_error("project run evidence", error))?;
            affected.insert(fact.run.as_bytes().to_vec());
        }

        let mut changes = Vec::with_capacity(affected.len());
        for run_key in affected {
            let state = reduce_run_state(transaction, &run_key, context.commit_seq)?;
            changes.push(state_change(&run_key, state.as_deref())?);
        }
        Ok(changes)
    }

    fn apply_usage(
        &self,
        transaction: &Transaction<'_>,
        context: &ProjectionCommitContext,
    ) -> Result<Vec<ChangeEntry>, EngineError> {
        let mut touched_sessions = BTreeSet::new();
        let replaced = read_replaced_contributions(transaction, context)?;
        for contribution in &replaced {
            adjust_usage_total(transaction, contribution, -1, context.commit_seq)?;
            touched_sessions.insert(contribution.session_key.clone());
        }
        transaction
            .execute(
                "DELETE FROM usage_contributions WHERE source_object_id = ?1 AND source_generation <> ?2",
                params![
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.generation, "source generation")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced usage contributions", error))?;

        for envelope in self.batch.facts() {
            let Fact::Usage(fact) = &envelope.value else {
                continue;
            };
            if fact.accounting != UsageAccounting::Delta {
                return Err(EngineError::InvalidCommit(format!(
                    "{} usage requires a declared counter series; Phase 4 only projects Delta accounting",
                    usage_accounting(fact.accounting)
                )));
            }
            if let Some(previous) = read_contribution(transaction, envelope.id.as_bytes())? {
                adjust_usage_total(transaction, &previous, -1, context.commit_seq)?;
                touched_sessions.insert(previous.session_key);
            }
            let contribution = UsageContribution::from_fact(fact)?;
            write_contribution(transaction, context, envelope, &contribution)?;
            adjust_usage_total(transaction, &contribution, 1, context.commit_seq)?;
            touched_sessions.insert(contribution.session_key);
        }

        // Generation-owned fact rows are removed only after every projector
        // has retracted its dependent state and usage delta.
        transaction
            .execute(
                "DELETE FROM fact_records WHERE source_object_id = ?1 AND source_generation <> ?2",
                params![
                    sqlite_u64(context.source_object_id, "source object id")?,
                    sqlite_u64(context.generation, "source generation")?,
                ],
            )
            .map_err(|error| sqlite_error("retract replaced fact records", error))?;

        touched_sessions
            .into_iter()
            .map(|session_key| {
                let exists = transaction
                    .query_row(
                        "SELECT 1 FROM usage_totals WHERE session_key = ?1",
                        [&session_key],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(|error| sqlite_error("read usage total change", error))?
                    .is_some();
                if exists {
                    simple_change("usage.session.changed", &session_key, "upsert", None)
                } else {
                    simple_change("usage.session.changed", &session_key, "delete", None)
                }
            })
            .collect()
    }
}

fn validate_batch_provenance(
    batch: &FactBatch,
    context: &ProjectionCommitContext,
) -> Result<(), EngineError> {
    for envelope in batch.facts() {
        let provenance = &envelope.provenance;
        if provenance.source_instance_id != context.source_instance_id
            || provenance.stream_id != context.source_stream_id
            || provenance.object_id != context.source_object_id
            || provenance.generation != context.generation
        {
            return Err(EngineError::InvalidCommit(format!(
                "fact {:?} provenance does not match the reserved source object",
                envelope.id
            )));
        }
        if provenance.cursor_end.is_empty() || provenance.record_hash.iter().all(|byte| *byte == 0)
        {
            return Err(EngineError::InvalidCommit(format!(
                "fact {:?} has incomplete source provenance",
                envelope.id
            )));
        }
    }
    Ok(())
}

fn persist_fact(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
) -> Result<(), EngineError> {
    let payload = serialize(&envelope.value, "serialize fact audit payload")?;
    transaction
        .execute(
            r#"
            INSERT INTO fact_records (
                fact_id, fact_kind, entity_key, source_instance_id,
                source_stream_id, source_object_id, source_generation,
                cursor_start, cursor_end, payload_hash, local_fact_ordinal,
                observed_at, payload_json, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14
            )
            ON CONFLICT(fact_id) DO UPDATE SET
                fact_kind = excluded.fact_kind,
                entity_key = excluded.entity_key,
                source_instance_id = excluded.source_instance_id,
                source_stream_id = excluded.source_stream_id,
                source_object_id = excluded.source_object_id,
                source_generation = excluded.source_generation,
                cursor_start = excluded.cursor_start,
                cursor_end = excluded.cursor_end,
                payload_hash = excluded.payload_hash,
                local_fact_ordinal = excluded.local_fact_ordinal,
                observed_at = excluded.observed_at,
                payload_json = excluded.payload_json,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                envelope.value.kind(),
                envelope.value.entity_key().map(EntityKey::as_bytes),
                sqlite_u64(context.source_instance_id, "source instance id")?,
                sqlite_u64(context.source_stream_id, "source stream id")?,
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_start,
                envelope.provenance.cursor_end,
                envelope.provenance.record_hash.as_slice(),
                i64::from(envelope.provenance.local_fact_ordinal),
                envelope.provenance.observed_at,
                payload,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("persist typed fact", error))
}

fn retract_canonical_generation(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let generation = sqlite_u64(context.generation, "source generation")?;
    let mut changes = Vec::new();
    for (table, key_column, topic, operation) in [
        (
            "canonical_messages",
            "message_key",
            "history.message.changed",
            "retract canonical messages",
        ),
        (
            "canonical_runs",
            "run_key",
            "history.run.changed",
            "retract canonical runs",
        ),
        (
            "canonical_sessions",
            "session_key",
            "history.session.changed",
            "retract canonical sessions",
        ),
    ] {
        let select_sql = format!(
            "SELECT {key_column} FROM {table} WHERE source_object_id = ?1 AND source_generation <> ?2"
        );
        let mut statement = transaction
            .prepare(&select_sql)
            .map_err(|error| sqlite_error("prepare canonical generation retraction", error))?;
        let keys = statement
            .query_map(params![object_id, generation], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .map_err(|error| sqlite_error("read canonical generation retraction", error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| sqlite_error("collect canonical generation retraction", error))?;
        drop(statement);
        for key in keys {
            changes.push(simple_change(topic, &key, "delete", None)?);
        }
        let delete_sql =
            format!("DELETE FROM {table} WHERE source_object_id = ?1 AND source_generation <> ?2");
        transaction
            .execute(&delete_sql, params![object_id, generation])
            .map_err(|error| sqlite_error(operation, error))?;
    }
    Ok(changes)
}

fn old_generation_keys(
    transaction: &Transaction<'_>,
    sql: &str,
    context: &ProjectionCommitContext,
    operation: &'static str,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| sqlite_error(operation, error))?;
    let keys = statement
        .query_map(
            params![
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .map_err(|error| sqlite_error(operation, error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error(operation, error))?;
    Ok(keys)
}

fn reduce_run_state(
    transaction: &Transaction<'_>,
    run_key: &[u8],
    commit_seq: u64,
) -> Result<Option<String>, EngineError> {
    let decisive = transaction
        .query_row(
            r#"
            SELECT fact_id, evidence_kind, source_time
            FROM run_evidence
            WHERE run_key = ?1
            ORDER BY
              CASE evidence_kind
                WHEN 'terminal_succeeded' THEN 60
                WHEN 'terminal_failed' THEN 60
                WHEN 'terminal_cancelled' THEN 60
                WHEN 'input_requested' THEN 50
                WHEN 'waiting_observed' THEN 45
                WHEN 'run_started' THEN 40
                WHEN 'activity_observed' THEN 35
                WHEN 'run_declared' THEN 20
                ELSE 0
              END DESC,
              CASE evidence_strength
                WHEN 'native_explicit' THEN 40
                WHEN 'native_activity' THEN 30
                WHEN 'presence' THEN 20
                WHEN 'layout' THEN 10
                ELSE 0
              END DESC,
              source_generation DESC, cursor_end DESC, last_commit_seq DESC,
              fact_id DESC
            LIMIT 1
            "#,
            [run_key],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_error("reduce observed run state", error))?;
    let Some((decisive_evidence_id, kind, source_time)) = decisive else {
        transaction
            .execute(
                "DELETE FROM observed_run_states WHERE run_key = ?1",
                [run_key],
            )
            .map_err(|error| sqlite_error("remove empty observed run state", error))?;
        return Ok(None);
    };
    let state = match kind.as_str() {
        "terminal_succeeded" => "succeeded",
        "terminal_failed" => "failed",
        "terminal_cancelled" => "cancelled",
        "input_requested" | "waiting_observed" => "waiting",
        "run_started" | "activity_observed" => "active",
        "run_declared" => "declared",
        _ => "unknown",
    };
    let last_activity_at = transaction
        .query_row(
            r#"
            SELECT MAX(source_time) FROM run_evidence
            WHERE run_key = ?1
              AND evidence_kind IN ('run_started', 'activity_observed')
            "#,
            [run_key],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(|error| sqlite_error("read run activity time", error))?;
    let terminal_at = matches!(state, "succeeded" | "failed" | "cancelled")
        .then_some(source_time)
        .flatten();
    transaction
        .execute(
            r#"
            INSERT INTO observed_run_states (
                run_key, state, decisive_evidence_id, last_activity_at,
                terminal_at, last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(run_key) DO UPDATE SET
                state = excluded.state,
                decisive_evidence_id = excluded.decisive_evidence_id,
                last_activity_at = excluded.last_activity_at,
                terminal_at = excluded.terminal_at,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                run_key,
                state,
                decisive_evidence_id,
                last_activity_at,
                terminal_at,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write observed run state", error))?;
    Ok(Some(state.to_string()))
}

#[derive(Debug, Clone)]
struct UsageContribution {
    session_key: Vec<u8>,
    subject_key: Vec<u8>,
    quality_bucket: &'static str,
    values: [i64; 4],
}

impl UsageContribution {
    fn from_fact(fact: &UsageFact) -> Result<Self, EngineError> {
        Ok(Self {
            session_key: fact.session.as_bytes().to_vec(),
            subject_key: fact.subject.as_bytes().to_vec(),
            quality_bucket: quality_bucket(fact.quality),
            values: token_values(fact.values)?,
        })
    }
}

fn read_replaced_contributions(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
) -> Result<Vec<UsageContribution>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT session_key, subject_key, quality_bucket, input_tokens,
                   output_tokens, cache_creation_tokens, cache_read_tokens
            FROM usage_contributions
            WHERE source_object_id = ?1 AND source_generation <> ?2
            "#,
        )
        .map_err(|error| sqlite_error("prepare replaced usage read", error))?;
    let contributions = statement
        .query_map(
            params![
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
            ],
            contribution_from_row,
        )
        .map_err(|error| sqlite_error("read replaced usage contributions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect replaced usage contributions", error))?;
    Ok(contributions)
}

fn read_contribution(
    transaction: &Transaction<'_>,
    fact_id: &[u8],
) -> Result<Option<UsageContribution>, EngineError> {
    transaction
        .query_row(
            r#"
            SELECT session_key, subject_key, quality_bucket, input_tokens,
                   output_tokens, cache_creation_tokens, cache_read_tokens
            FROM usage_contributions WHERE fact_id = ?1
            "#,
            [fact_id],
            contribution_from_row,
        )
        .optional()
        .map_err(|error| sqlite_error("read prior usage contribution", error))
}

fn contribution_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageContribution> {
    let bucket: String = row.get(2)?;
    let quality_bucket = match bucket.as_str() {
        "exact" => "exact",
        _ => "estimated",
    };
    Ok(UsageContribution {
        session_key: row.get(0)?,
        subject_key: row.get(1)?,
        quality_bucket,
        values: [row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?],
    })
}

fn write_contribution(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    contribution: &UsageContribution,
) -> Result<(), EngineError> {
    let Fact::Usage(fact) = &envelope.value else {
        return Err(EngineError::InvalidCommit(
            "non-usage fact reached usage projector".to_string(),
        ));
    };
    transaction
        .execute(
            r#"
            INSERT INTO usage_contributions (
                fact_id, subject_key, session_key, scope, accounting,
                quality, quality_bucket, input_tokens, output_tokens,
                cache_creation_tokens, cache_read_tokens, model, source_time,
                source_time_quality, source_object_id, source_generation,
                cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18
            )
            ON CONFLICT(fact_id) DO UPDATE SET
                subject_key = excluded.subject_key,
                session_key = excluded.session_key,
                scope = excluded.scope,
                accounting = excluded.accounting,
                quality = excluded.quality,
                quality_bucket = excluded.quality_bucket,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                cache_creation_tokens = excluded.cache_creation_tokens,
                cache_read_tokens = excluded.cache_read_tokens,
                model = excluded.model,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                source_object_id = excluded.source_object_id,
                source_generation = excluded.source_generation,
                cursor_end = excluded.cursor_end,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                contribution.subject_key,
                contribution.session_key,
                usage_scope(fact.scope),
                usage_accounting(fact.accounting),
                value_quality(fact.quality),
                contribution.quality_bucket,
                contribution.values[0],
                contribution.values[1],
                contribution.values[2],
                contribution.values[3],
                fact.model,
                timestamp_value(fact.source_time.as_ref()),
                timestamp_quality(fact.source_time.as_ref()),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("write usage contribution", error))
}

fn adjust_usage_total(
    transaction: &Transaction<'_>,
    contribution: &UsageContribution,
    direction: i64,
    commit_seq: u64,
) -> Result<(), EngineError> {
    let current = transaction
        .query_row(
            r#"
            SELECT exact_input_tokens, exact_output_tokens,
                   exact_cache_creation_tokens, exact_cache_read_tokens,
                   estimated_input_tokens, estimated_output_tokens,
                   estimated_cache_creation_tokens, estimated_cache_read_tokens
            FROM usage_totals WHERE session_key = ?1
            "#,
            [&contribution.session_key],
            |row| {
                Ok([
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ])
            },
        )
        .optional()
        .map_err(|error| sqlite_error("read usage aggregate", error))?
        .unwrap_or([0_i64; 8]);
    let offset = usize::from(contribution.quality_bucket != "exact") * 4;
    let mut next = current;
    for (index, value) in contribution.values.iter().enumerate() {
        let delta = value
            .checked_mul(direction)
            .ok_or_else(|| EngineError::InvalidCommit("usage delta overflow".to_string()))?;
        next[offset + index] = current[offset + index]
            .checked_add(delta)
            .filter(|total| *total >= 0)
            .ok_or_else(|| {
                EngineError::InvalidCommit(
                    "usage contribution would make an aggregate negative".to_string(),
                )
            })?;
    }
    if next.iter().all(|value| *value == 0) {
        transaction
            .execute(
                "DELETE FROM usage_totals WHERE session_key = ?1",
                [&contribution.session_key],
            )
            .map_err(|error| sqlite_error("remove empty usage aggregate", error))?;
        return Ok(());
    }
    transaction
        .execute(
            r#"
            INSERT INTO usage_totals (
                session_key, exact_input_tokens, exact_output_tokens,
                exact_cache_creation_tokens, exact_cache_read_tokens,
                estimated_input_tokens, estimated_output_tokens,
                estimated_cache_creation_tokens, estimated_cache_read_tokens,
                last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(session_key) DO UPDATE SET
                exact_input_tokens = excluded.exact_input_tokens,
                exact_output_tokens = excluded.exact_output_tokens,
                exact_cache_creation_tokens = excluded.exact_cache_creation_tokens,
                exact_cache_read_tokens = excluded.exact_cache_read_tokens,
                estimated_input_tokens = excluded.estimated_input_tokens,
                estimated_output_tokens = excluded.estimated_output_tokens,
                estimated_cache_creation_tokens = excluded.estimated_cache_creation_tokens,
                estimated_cache_read_tokens = excluded.estimated_cache_read_tokens,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                contribution.session_key,
                next[0],
                next[1],
                next[2],
                next[3],
                next[4],
                next[5],
                next[6],
                next[7],
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("apply usage aggregate delta", error))
}

/// Repair path retained for audit/migration. The ingest hot path never calls
/// this full rebuild; it updates only contributions changed by the commit.
pub(crate) fn rebuild_usage_totals_for_audit(
    connection: &mut Connection,
) -> Result<(), EngineError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_error("begin usage audit rebuild", error))?;
    transaction
        .execute("DELETE FROM usage_totals", [])
        .map_err(|error| sqlite_error("clear usage audit totals", error))?;
    transaction
        .execute(
            r#"
            INSERT INTO usage_totals (
                session_key, exact_input_tokens, exact_output_tokens,
                exact_cache_creation_tokens, exact_cache_read_tokens,
                estimated_input_tokens, estimated_output_tokens,
                estimated_cache_creation_tokens, estimated_cache_read_tokens,
                last_commit_seq
            )
            SELECT
                session_key,
                SUM(CASE WHEN quality_bucket = 'exact' THEN input_tokens ELSE 0 END),
                SUM(CASE WHEN quality_bucket = 'exact' THEN output_tokens ELSE 0 END),
                SUM(CASE WHEN quality_bucket = 'exact' THEN cache_creation_tokens ELSE 0 END),
                SUM(CASE WHEN quality_bucket = 'exact' THEN cache_read_tokens ELSE 0 END),
                SUM(CASE WHEN quality_bucket = 'estimated' THEN input_tokens ELSE 0 END),
                SUM(CASE WHEN quality_bucket = 'estimated' THEN output_tokens ELSE 0 END),
                SUM(CASE WHEN quality_bucket = 'estimated' THEN cache_creation_tokens ELSE 0 END),
                SUM(CASE WHEN quality_bucket = 'estimated' THEN cache_read_tokens ELSE 0 END),
                MAX(last_commit_seq)
            FROM usage_contributions
            GROUP BY session_key
            "#,
            [],
        )
        .map_err(|error| sqlite_error("rebuild usage audit totals", error))?;
    transaction
        .commit()
        .map_err(|error| sqlite_error("commit usage audit rebuild", error))
}

fn upsert_change(
    topic: &'static str,
    entity_key: &[u8],
    envelope: &FactEnvelope,
) -> Result<ChangeEntry, EngineError> {
    simple_change(topic, entity_key, "upsert", Some(envelope))
}

fn state_change(entity_key: &[u8], state: Option<&str>) -> Result<ChangeEntry, EngineError> {
    let payload = serialize(
        &serde_json::json!({ "state": state }),
        "serialize runtime change",
    )?;
    Ok(ChangeEntry {
        topic: "runtime.run.changed".to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if state.is_some() { "upsert" } else { "delete" }.to_string(),
        payload,
    })
}

fn simple_change(
    topic: &'static str,
    entity_key: &[u8],
    operation: &'static str,
    envelope: Option<&FactEnvelope>,
) -> Result<ChangeEntry, EngineError> {
    let payload = match envelope {
        Some(envelope) => serialize(
            &serde_json::json!({
                "fact_id": hex(envelope.id.as_bytes()),
                "fact_kind": envelope.value.kind(),
            }),
            "serialize projection change",
        )?,
        None => b"{}".to_vec(),
    };
    Ok(ChangeEntry {
        topic: topic.to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: operation.to_string(),
        payload,
    })
}

fn serialize<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> Result<Vec<u8>, EngineError> {
    serde_json::to_vec(value)
        .map_err(|error| EngineError::InvalidCommit(format!("{operation}: {error}")))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn token_values(values: TokenUsage) -> Result<[i64; 4], EngineError> {
    Ok([
        sqlite_u64(values.input_tokens, "input tokens")?,
        sqlite_u64(values.output_tokens, "output tokens")?,
        sqlite_u64(values.cache_creation_tokens, "cache creation tokens")?,
        sqlite_u64(values.cache_read_tokens, "cache read tokens")?,
    ])
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

fn message_role(role: &MessageRole) -> &str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Summary => "summary",
        MessageRole::Other(value) => value,
    }
}

fn evidence_kind(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::RunDeclared => "run_declared",
        EvidenceKind::RunStarted => "run_started",
        EvidenceKind::ActivityObserved => "activity_observed",
        EvidenceKind::WaitingObserved => "waiting_observed",
        EvidenceKind::InputRequested => "input_requested",
        EvidenceKind::TerminalSucceeded => "terminal_succeeded",
        EvidenceKind::TerminalFailed => "terminal_failed",
        EvidenceKind::TerminalCancelled => "terminal_cancelled",
    }
}

fn evidence_strength(strength: EvidenceStrength) -> &'static str {
    match strength {
        EvidenceStrength::Layout => "layout",
        EvidenceStrength::Presence => "presence",
        EvidenceStrength::NativeActivity => "native_activity",
        EvidenceStrength::NativeExplicit => "native_explicit",
    }
}

fn usage_scope(scope: UsageScope) -> &'static str {
    match scope {
        UsageScope::Record => "record",
        UsageScope::Message => "message",
        UsageScope::Turn => "turn",
        UsageScope::Run => "run",
        UsageScope::Session => "session",
        UsageScope::Team => "team",
        UsageScope::Project => "project",
    }
}

fn usage_accounting(accounting: UsageAccounting) -> &'static str {
    match accounting {
        UsageAccounting::Delta => "delta",
        UsageAccounting::Cumulative => "cumulative",
        UsageAccounting::Snapshot => "snapshot",
    }
}

fn value_quality(quality: ValueQuality) -> &'static str {
    match quality {
        ValueQuality::NativeExact => "native_exact",
        ValueQuality::NativeApproximate => "native_approximate",
        ValueQuality::DerivedExact => "derived_exact",
        ValueQuality::Estimated => "estimated",
    }
}

fn quality_bucket(quality: ValueQuality) -> &'static str {
    match quality {
        ValueQuality::NativeExact | ValueQuality::DerivedExact => "exact",
        ValueQuality::NativeApproximate | ValueQuality::Estimated => "estimated",
    }
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;

    use tempfile::TempDir;
    use walkdir::WalkDir;

    use crate::adapter::{
        AdapterObjectContext, AgentAdapter, DecodeContext, DecoderId, FactBatch, SourceInstance,
        SourceInstanceKey, SourceInstanceSpec as AdapterSourceInstanceSpec, SourceObjectDescriptor,
        SourceRoot, StreamId,
    };
    use crate::claude::ClaudeCodeAdapter;
    use crate::core::schema;
    use crate::orchestrate::ingest::{run_ingest, IngestOptions};
    use crate::source::{
        AppendCheckpoint, AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead,
        RecordOrigin, SourceCursor, SourceMediaType, SourceRecord,
    };

    use super::*;
    use crate::engine::commit::{
        apply_observation_commit, ExpectedSourceCursor, SourceInstanceSpec, SourceObjectUpdate,
        SourceStreamSpec,
    };

    const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const PROJECT: &str = "-Users-fixture-project";
    const STREAM: &str = "session-transcripts";
    const DECODER: &str = "claude-session-record";

    fn transcript() -> Vec<Vec<u8>> {
        vec![
            format!(
                r#"{{"type":"assistant","uuid":"m1","parentUuid":"u1","timestamp":"2026-08-11T00:00:00Z","sessionId":"{SESSION}","cwd":"/fixture/project","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"r1","message":{{"model":"claude-sonnet","id":"api1","type":"message","role":"assistant","content":[{{"type":"text","text":"hello"}}],"usage":{{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":2,"cache_read_input_tokens":3}}}}}}"#
            )
            .into_bytes(),
            format!(
                r#"{{"type":"assistant","uuid":"m2","parentUuid":"m1","timestamp":"2026-08-11T00:00:01Z","sessionId":"{SESSION}","cwd":"/fixture/project","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"r2","message":{{"model":"claude-sonnet","id":"api2","type":"message","role":"assistant","content":[{{"type":"text","text":"world"}}],"usage":{{"input_tokens":7,"output_tokens":4,"cache_creation_input_tokens":0,"cache_read_input_tokens":1}}}}}}"#
            )
            .into_bytes(),
        ]
    }

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        schema::initialize_schema(&connection).unwrap();
        connection
    }

    fn adapter_context(root: &Path, adapter: &ClaudeCodeAdapter) -> AdapterObjectContext {
        adapter
            .bootstrap_object(
                &SourceInstance {
                    id: 1,
                    spec: AdapterSourceInstanceSpec {
                        stable_key: SourceInstanceKey::new(b"fixture-root".to_vec()).unwrap(),
                        display_name: "fixture".to_string(),
                        roots: vec![SourceRoot {
                            name: "projects".to_string(),
                            path: root.to_path_buf(),
                        }],
                        discovery_reason: "fixture".to_string(),
                    },
                },
                &SourceObjectDescriptor {
                    stream_id: StreamId::new(STREAM).unwrap(),
                    object_key: b"fixture-transcript".to_vec(),
                    relative_path: Path::new(&format!("{PROJECT}/{SESSION}.jsonl")).to_path_buf(),
                },
            )
            .unwrap()
    }

    fn request(
        expected: ExpectedSourceCursor,
        generation: u64,
        committed_cursor: Vec<u8>,
        started_at: i64,
    ) -> ObservationCommit {
        ObservationCommit {
            source: SourceInstanceSpec {
                adapter_id: "claude-code".to_string(),
                stable_key: b"fixture-root".to_vec(),
                display_name: "Claude fixture".to_string(),
                adapter_contract_version: 1,
                discovered_at: 1,
                last_seen_at: started_at,
            },
            stream: SourceStreamSpec {
                stream_key: STREAM.to_string(),
                driver_kind: "append_delimited_file".to_string(),
                decoder_key: DECODER.to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: Some(started_at),
            },
            object: SourceObjectUpdate {
                object_key: b"fixture-transcript".to_vec(),
                expected,
                display_path: Some(format!("{PROJECT}/{SESSION}.jsonl")),
                native_identity: None,
                generation,
                committed_cursor,
                observed_revision: None,
                adapter_object_context: None,
                decoder_state: None,
                decoder_state_version: None,
                size_bytes: None,
                mtime_ns: None,
                decoder_contract_version: 1,
                state: "active".to_string(),
            },
            reason: "fixture".to_string(),
            started_at,
            committed_at: started_at + 1,
            fact_count: 0,
            projection_versions: Vec::new(),
            record_errors: Vec::new(),
            changes: Vec::new(),
        }
    }

    fn register_object(connection: &mut Connection) {
        apply_observation_commit(
            connection,
            &request(
                ExpectedSourceCursor::Absent,
                1,
                SourceCursor::append_offset(0).into_bytes(),
                10,
            ),
        )
        .unwrap();
    }

    fn origin(observed_at: i64) -> RecordOrigin {
        RecordOrigin {
            source_instance_id: 1,
            stream_id: 1,
            object_id: 1,
            observed_at,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        }
    }

    fn decode_commit(
        connection: &mut Connection,
        adapter: &ClaudeCodeAdapter,
        object_context: &AdapterObjectContext,
        record: &SourceRecord,
        expected_generation: u64,
        expected_cursor: Vec<u8>,
        clock: i64,
    ) -> Vec<u8> {
        let decoder = DecoderId::new(DECODER).unwrap();
        let mut batch = FactBatch::new(16, 8).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context,
                },
                record,
                &mut batch,
            )
            .unwrap();
        let committed_cursor = record.cursor_end.as_bytes().to_vec();
        let receipt = apply_fact_observation_commit(
            connection,
            &request(
                ExpectedSourceCursor::At {
                    generation: expected_generation,
                    committed_cursor: expected_cursor,
                },
                record.generation,
                committed_cursor.clone(),
                clock,
            ),
            &batch,
        )
        .unwrap();
        assert!(receipt.change_count >= 4);
        committed_cursor
    }

    fn append_records(read: AppendRead) -> (Vec<SourceRecord>, AppendCheckpoint) {
        let AppendRead::Batch {
            items, checkpoint, ..
        } = read
        else {
            panic!("expected append batch")
        };
        let records = items
            .into_iter()
            .map(|item| match item {
                AppendItem::Record(record) => record,
                AppendItem::Quarantined(error) => panic!("unexpected quarantine: {error:?}"),
            })
            .collect();
        (records, checkpoint)
    }

    fn ingest_cold() -> (Connection, TempDir, AppendCheckpoint) {
        let root = TempDir::new().unwrap();
        let path = root.path().join("transcript.jsonl");
        let mut bytes = Vec::new();
        for line in transcript() {
            bytes.extend(line);
            bytes.push(b'\n');
        }
        std::fs::write(&path, bytes).unwrap();
        let driver = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
        let (records, checkpoint) =
            append_records(driver.read(&path, None, &origin(20), false).unwrap());
        let adapter = ClaudeCodeAdapter::new();
        let context = adapter_context(root.path(), &adapter);
        let mut connection = database();
        register_object(&mut connection);
        let mut cursor = SourceCursor::append_offset(0).into_bytes();
        for (index, record) in records.iter().enumerate() {
            cursor = decode_commit(
                &mut connection,
                &adapter,
                &context,
                record,
                1,
                cursor,
                30 + index as i64,
            );
        }
        (connection, root, checkpoint)
    }

    fn ingest_live() -> Connection {
        let root = TempDir::new().unwrap();
        let path = root.path().join("transcript.jsonl");
        std::fs::write(&path, []).unwrap();
        let driver = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let context = adapter_context(root.path(), &adapter);
        let mut connection = database();
        register_object(&mut connection);
        let mut checkpoint = None;
        let mut cursor = SourceCursor::append_offset(0).into_bytes();
        for (index, line) in transcript().into_iter().enumerate() {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(&line).unwrap();
            file.write_all(b"\n").unwrap();
            file.sync_all().unwrap();
            let (records, next_checkpoint) = append_records(
                driver
                    .read(
                        &path,
                        checkpoint.as_ref(),
                        &origin(40 + index as i64),
                        false,
                    )
                    .unwrap(),
            );
            assert_eq!(records.len(), 1);
            cursor = decode_commit(
                &mut connection,
                &adapter,
                &context,
                &records[0],
                1,
                cursor,
                50 + index as i64,
            );
            checkpoint = Some(next_checkpoint);
        }
        connection
    }

    type MessageSnapshot = (String, String, Option<String>, Option<String>, Vec<u8>);

    #[derive(Debug, PartialEq, Eq)]
    struct SemanticSnapshot {
        sessions: Vec<(String, String, Option<String>, Option<String>)>,
        messages: Vec<MessageSnapshot>,
        runs: Vec<(String, Option<Vec<u8>>)>,
        states: Vec<String>,
        totals: Vec<[i64; 8]>,
    }

    fn semantic_snapshot(connection: &Connection) -> SemanticSnapshot {
        fn collect<T, F>(connection: &Connection, sql: &str, map: F) -> Vec<T>
        where
            F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
        {
            connection
                .prepare(sql)
                .unwrap()
                .query_map([], map)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        }
        SemanticSnapshot {
            sessions: collect(
                connection,
                "SELECT native_session_id, native_project_key, cwd, git_branch FROM canonical_sessions ORDER BY session_key",
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ),
            messages: collect(
                connection,
                "SELECT native_kind, role, native_message_id, source_time, content_json FROM canonical_messages ORDER BY cursor_start",
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ),
            runs: collect(
                connection,
                "SELECT native_run_id, parent_run_key FROM canonical_runs ORDER BY run_key",
                |row| Ok((row.get(0)?, row.get(1)?)),
            ),
            states: collect(
                connection,
                "SELECT state FROM observed_run_states ORDER BY run_key",
                |row| row.get(0),
            ),
            totals: collect(
                connection,
                r#"SELECT exact_input_tokens, exact_output_tokens,
                          exact_cache_creation_tokens, exact_cache_read_tokens,
                          estimated_input_tokens, estimated_output_tokens,
                          estimated_cache_creation_tokens, estimated_cache_read_tokens
                   FROM usage_totals ORDER BY session_key"#,
                |row| {
                    Ok([
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                        row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                    ])
                },
            ),
        }
    }

    fn count(connection: &Connection, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        connection.query_row(&sql, [], |row| row.get(0)).unwrap()
    }

    struct ShadowCommit<'a> {
        stream: &'a str,
        decoder: &'a str,
        object_key: &'a [u8],
        expected: ExpectedSourceCursor,
        generation: u64,
        committed_cursor: Vec<u8>,
        clock: i64,
        object_context: Option<&'a AdapterObjectContext>,
    }

    fn shadow_request(input: ShadowCommit<'_>) -> ObservationCommit {
        ObservationCommit {
            source: SourceInstanceSpec {
                adapter_id: "claude-code".to_string(),
                stable_key: b"phase4-shadow-fixture".to_vec(),
                display_name: "Claude Phase 4 shadow fixture".to_string(),
                adapter_contract_version: 1,
                discovered_at: 1,
                last_seen_at: input.clock,
            },
            stream: SourceStreamSpec {
                stream_key: input.stream.to_string(),
                driver_kind: "append_delimited_file".to_string(),
                decoder_key: input.decoder.to_string(),
                stream_state: "available".to_string(),
                last_reconciled_at: Some(input.clock),
            },
            object: SourceObjectUpdate {
                object_key: input.object_key.to_vec(),
                expected: input.expected,
                display_path: Some(String::from_utf8_lossy(input.object_key).into_owned()),
                native_identity: None,
                generation: input.generation,
                committed_cursor: input.committed_cursor,
                observed_revision: None,
                adapter_object_context: input.object_context.map(|value| value.payload().to_vec()),
                decoder_state: None,
                decoder_state_version: None,
                size_bytes: None,
                mtime_ns: None,
                decoder_contract_version: 1,
                state: "active".to_string(),
            },
            reason: "phase4_shadow".to_string(),
            started_at: input.clock,
            committed_at: input.clock + 1,
            fact_count: 0,
            projection_versions: Vec::new(),
            record_errors: Vec::new(),
            changes: Vec::new(),
        }
    }

    fn shadow_ingest_fixture(root: &Path) -> Connection {
        let projects = root.join("projects");
        let adapter = ClaudeCodeAdapter::new();
        let mut connection = database();
        let mut objects = WalkDir::new(&projects)
            .follow_links(false)
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| {
                let relative = entry.path().strip_prefix(&projects).unwrap().to_path_buf();
                let components = relative
                    .components()
                    .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
                    .collect::<Vec<_>>();
                let file_name = components.last()?;
                let stream = if components.len() == 2
                    && file_name.ends_with(".jsonl")
                    && file_name != "sessions-index.json"
                {
                    Some((STREAM, DECODER))
                } else if components.len() >= 4
                    && components.get(2).map(String::as_str) == Some("subagents")
                    && file_name.starts_with("agent-")
                    && file_name.ends_with(".jsonl")
                {
                    Some(("subagent-transcripts", "claude-subagent-record"))
                } else {
                    None
                }?;
                Some((relative, entry.path().to_path_buf(), stream))
            })
            .collect::<Vec<_>>();
        objects.sort_by(|left, right| left.0.cmp(&right.0));

        let driver = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
        let mut clock = 100_i64;
        for (relative, path, (stream, decoder)) in objects {
            let object_key = relative.to_string_lossy().as_bytes().to_vec();
            let registration = apply_observation_commit(
                &mut connection,
                &shadow_request(ShadowCommit {
                    stream,
                    decoder,
                    object_key: &object_key,
                    expected: ExpectedSourceCursor::Absent,
                    generation: 1,
                    committed_cursor: SourceCursor::append_offset(0).into_bytes(),
                    clock,
                    object_context: None,
                }),
            )
            .unwrap();
            clock += 2;
            let instance = SourceInstance {
                id: registration.source_instance_id,
                spec: AdapterSourceInstanceSpec {
                    stable_key: SourceInstanceKey::new(b"phase4-shadow-fixture".to_vec()).unwrap(),
                    display_name: "shadow".to_string(),
                    roots: vec![SourceRoot {
                        name: "projects".to_string(),
                        path: projects.clone(),
                    }],
                    discovery_reason: "fixture".to_string(),
                },
            };
            let object_context = adapter
                .bootstrap_object(
                    &instance,
                    &SourceObjectDescriptor {
                        stream_id: StreamId::new(stream).unwrap(),
                        object_key: object_key.clone(),
                        relative_path: relative,
                    },
                )
                .unwrap();
            let decoder_id = DecoderId::new(decoder).unwrap();
            let origin = RecordOrigin {
                source_instance_id: registration.source_instance_id,
                stream_id: registration.source_stream_id,
                object_id: registration.source_object_id,
                observed_at: clock,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
            };
            let (records, _) = append_records(driver.read(&path, None, &origin, false).unwrap());
            let mut cursor = SourceCursor::append_offset(0).into_bytes();
            for record in records {
                let mut batch = FactBatch::new(16, 8).unwrap();
                adapter
                    .decode(
                        DecodeContext {
                            decoder: &decoder_id,
                            object_context: &object_context,
                        },
                        &record,
                        &mut batch,
                    )
                    .unwrap();
                let next_cursor = record.cursor_end.as_bytes().to_vec();
                apply_fact_observation_commit(
                    &mut connection,
                    &shadow_request(ShadowCommit {
                        stream,
                        decoder,
                        object_key: &object_key,
                        expected: ExpectedSourceCursor::At {
                            generation: 1,
                            committed_cursor: cursor,
                        },
                        generation: 1,
                        committed_cursor: next_cursor.clone(),
                        clock,
                        object_context: Some(&object_context),
                    }),
                    &batch,
                )
                .unwrap();
                cursor = next_cursor;
                clock += 2;
            }
        }
        connection
    }

    type HistoryParityRow = (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        i64,
        i64,
        i64,
        i64,
        String,
    );

    fn normalized_json(raw: String) -> String {
        serde_json::to_string(&serde_json::from_str::<serde_json::Value>(&raw).unwrap()).unwrap()
    }

    type SessionParityRow = (String, String, String, String, String);

    fn legacy_session_rows(connection: &Connection) -> Vec<SessionParityRow> {
        let mut rows = connection
            .prepare(
                r#"
                SELECT id, project_slug, COALESCE(first_prompt, ''), ai_title,
                       custom_title
                FROM sessions WHERE source_id = 'claude-code'
                "#,
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        rows
    }

    fn shadow_session_rows(connection: &Connection) -> Vec<SessionParityRow> {
        let mut rows = connection
            .prepare(
                r#"
                SELECT native_session_id, native_project_key,
                       COALESCE(first_prompt, ''), COALESCE(ai_title, ''),
                       COALESCE(custom_title, '')
                FROM canonical_sessions
                "#,
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        rows
    }

    fn legacy_parent_rows(connection: &Connection) -> Vec<HistoryParityRow> {
        let mut rows = connection
            .prepare(
                r#"
                SELECT session_id, msg_type, uuid, timestamp, data,
                       input_tokens, output_tokens, cache_creation_tokens,
                       cache_read_tokens, text_content
                FROM messages WHERE source_id = 'claude-code'
                "#,
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    normalized_json(row.get(4)?),
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        rows
    }

    fn shadow_parent_rows(connection: &Connection) -> Vec<HistoryParityRow> {
        let mut rows = connection
            .prepare(
                r#"
                SELECT cs.native_session_id, cm.native_kind,
                       cm.native_message_id, cm.source_time,
                       CAST(cm.raw_json AS TEXT),
                       COALESCE(uc.input_tokens, 0),
                       COALESCE(uc.output_tokens, 0),
                       COALESCE(uc.cache_creation_tokens, 0),
                       COALESCE(uc.cache_read_tokens, 0),
                       COALESCE(cm.search_text, '')
                FROM canonical_messages cm
                JOIN canonical_sessions cs ON cs.session_key = cm.session_key
                JOIN source_objects so ON so.source_object_id = cm.source_object_id
                JOIN source_streams ss ON ss.source_stream_id = so.source_stream_id
                LEFT JOIN usage_contributions uc ON uc.subject_key = cm.message_key
                WHERE ss.stream_key = 'session-transcripts'
                "#,
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    normalized_json(row.get(4)?),
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        rows
    }

    type SubagentParityRow = (String, Option<String>, String, i64, i64, i64, i64);

    fn legacy_subagent_rows(connection: &Connection) -> Vec<SubagentParityRow> {
        let mut rows = connection
            .prepare(
                r#"
                SELECT session_id, timestamp, data, input_tokens, output_tokens,
                       cache_creation_tokens, cache_read_tokens
                FROM subagent_messages WHERE source_id = 'claude-code'
                "#,
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    normalized_json(row.get(2)?),
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        rows
    }

    fn shadow_subagent_rows(connection: &Connection) -> Vec<SubagentParityRow> {
        let mut rows = connection
            .prepare(
                r#"
                SELECT cs.native_session_id, cm.source_time,
                       CAST(cm.raw_json AS TEXT),
                       COALESCE(uc.input_tokens, 0),
                       COALESCE(uc.output_tokens, 0),
                       COALESCE(uc.cache_creation_tokens, 0),
                       COALESCE(uc.cache_read_tokens, 0)
                FROM canonical_messages cm
                JOIN canonical_sessions cs ON cs.session_key = cm.session_key
                JOIN source_objects so ON so.source_object_id = cm.source_object_id
                JOIN source_streams ss ON ss.source_stream_id = so.source_stream_id
                LEFT JOIN usage_contributions uc ON uc.subject_key = cm.message_key
                WHERE ss.stream_key = 'subagent-transcripts'
                "#,
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    normalized_json(row.get(2)?),
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        rows
    }

    #[test]
    fn claude_cold_live_and_generation_reconcile_converge_with_usage_deltas() {
        let (mut cold, root, checkpoint) = ingest_cold();
        let live = ingest_live();
        let baseline = semantic_snapshot(&cold);
        assert_eq!(baseline, semantic_snapshot(&live));
        assert_eq!(baseline.messages.len(), 2);
        assert_eq!(baseline.states, vec!["active"]);
        assert_eq!(baseline.totals, vec![[17, 9, 2, 4, 0, 0, 0, 0]]);

        let path = root.path().join("transcript.jsonl");
        let driver = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
        let (records, replay_checkpoint) = append_records(
            driver
                .read(&path, Some(&checkpoint), &origin(70), true)
                .unwrap(),
        );
        assert_eq!(replay_checkpoint.generation, 2);
        let adapter = ClaudeCodeAdapter::new();
        let context = adapter_context(root.path(), &adapter);
        let mut cursor = checkpoint.cursor().into_bytes();
        let mut expected_generation = 1;
        for (index, record) in records.iter().enumerate() {
            cursor = decode_commit(
                &mut cold,
                &adapter,
                &context,
                record,
                expected_generation,
                cursor,
                80 + index as i64,
            );
            expected_generation = 2;
        }
        assert_eq!(baseline, semantic_snapshot(&cold));
        assert_eq!(count(&cold, "canonical_messages"), 2);
        assert_eq!(count(&cold, "usage_contributions"), 2);
        assert_eq!(count(&cold, "fact_records"), 10);

        let before_audit = semantic_snapshot(&cold);
        rebuild_usage_totals_for_audit(&mut cold).unwrap();
        assert_eq!(before_audit, semantic_snapshot(&cold));
    }

    #[test]
    fn claude_shadow_history_and_usage_match_the_legacy_small_fixture() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/small/.claude");
        let legacy_dir = TempDir::new().unwrap();
        let legacy_path = legacy_dir.path().join("legacy.db");
        let stats = run_ingest(
            &IngestOptions {
                agent_dir: fixture.to_string_lossy().into_owned(),
                db_path: legacy_path.to_string_lossy().into_owned(),
                mode: "cold".to_string(),
                progress_interval_ms: None,
                parallelism: Some(1),
                source_id: Some("claude-code".to_string()),
                safe_bulk: Some(true),
            },
            None,
        )
        .unwrap();
        assert_eq!(stats.error_count, 0, "legacy fixture must be clean");
        let legacy = Connection::open(legacy_path).unwrap();
        let shadow = shadow_ingest_fixture(&fixture);

        assert_eq!(legacy_session_rows(&legacy), shadow_session_rows(&shadow));
        assert_eq!(legacy_parent_rows(&legacy), shadow_parent_rows(&shadow));
        assert_eq!(legacy_subagent_rows(&legacy), shadow_subagent_rows(&shadow));
        assert!(
            shadow
                .query_row(
                    "SELECT COUNT(*) FROM canonical_runs WHERE parent_run_key IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
                > 0
        );
        assert_eq!(
            count(&shadow, "usage_contributions"),
            count(&legacy, "messages") + count(&legacy, "subagent_messages")
                - legacy
                    .query_row(
                        r#"
                        SELECT COUNT(*) FROM (
                          SELECT input_tokens, output_tokens,
                                 cache_creation_tokens, cache_read_tokens
                          FROM messages
                          UNION ALL
                          SELECT input_tokens, output_tokens,
                                 cache_creation_tokens, cache_read_tokens
                          FROM subagent_messages
                        )
                        WHERE input_tokens = 0 AND output_tokens = 0
                          AND cache_creation_tokens = 0 AND cache_read_tokens = 0
                        "#,
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap()
        );
    }

    #[test]
    fn typed_commit_rejects_adapter_owned_change_topics() {
        let mut connection = database();
        register_object(&mut connection);
        let mut invalid = request(
            ExpectedSourceCursor::At {
                generation: 1,
                committed_cursor: SourceCursor::append_offset(0).into_bytes(),
            },
            1,
            SourceCursor::append_offset(1).into_bytes(),
            20,
        );
        invalid.changes.push(ChangeEntry {
            topic: "adapter.private".to_string(),
            schema_version: 1,
            entity_key: b"entity".to_vec(),
            operation: "upsert".to_string(),
            payload: Vec::new(),
        });
        let empty = FactBatch::new(1, 1).unwrap();
        assert!(matches!(
            apply_fact_observation_commit(&mut connection, &invalid, &empty),
            Err(EngineError::InvalidCommit(_))
        ));
        assert_eq!(count(&connection, "ingest_commits"), 1);
    }

    #[test]
    fn typed_commit_persists_batch_decoder_state_with_the_cursor() {
        let mut connection = database();
        register_object(&mut connection);
        let mut update = request(
            ExpectedSourceCursor::At {
                generation: 1,
                committed_cursor: SourceCursor::append_offset(0).into_bytes(),
            },
            1,
            SourceCursor::append_offset(1).into_bytes(),
            20,
        );
        update.object.decoder_state = Some(b"caller-state".to_vec());
        update.object.decoder_state_version = Some(3);
        let mut batch = FactBatch::new(1, 1).unwrap();
        batch
            .set_next_decoder_state(b"adapter-state".to_vec())
            .unwrap();

        apply_fact_observation_commit(&mut connection, &update, &batch).unwrap();
        let stored: (Vec<u8>, Vec<u8>, i64) = connection
            .query_row(
                "SELECT committed_cursor, decoder_state, decoder_state_version FROM source_objects",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored.0, SourceCursor::append_offset(1).into_bytes());
        assert_eq!(stored.1, b"adapter-state");
        assert_eq!(stored.2, 3);
    }
}
